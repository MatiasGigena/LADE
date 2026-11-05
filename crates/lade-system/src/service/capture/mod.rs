//! Working captures: snapshotting an Objective's worktree, registering the capture, and
//! continuing whatever request or event asked for it.

use std::path::PathBuf;

use lade_core::domain::ids::{CaptureId, DraftId, InputId, ObjectiveId};
use lade_protocol::client::{Event, FeedbackSubmitCommand, Rejection, Request};

use crate::storage::{self, CaptureRow};

use super::convert::wire_draft;
use super::convert::{not_found, wire_id};
use super::handlers::sql;
use super::jobs::{self, CaptureInput, CaptureOut, LatestCapture};
use super::state::ObjState;
use super::{Completed, ReqCtx, Service};
use crate::support::{new_uuid, now_ms};

pub(crate) enum CapturePurpose {
    Idle,
    Changes(ReqCtx),
    Retry(ReqCtx, Box<Request>),
    Submit(ReqCtx, Box<FeedbackSubmitCommand>),
    DispatchCheck(InputId),
}

impl Service {
    pub(crate) fn capture_input(
        &self,
        st: &ObjState,
        worktree: PathBuf,
        drafts: Vec<(DraftId, lade_core::domain::inspection::Reference)>,
    ) -> Result<CaptureInput, Rejection> {
        let ws = self.ws_of(st)?;
        Ok(CaptureInput {
            repo: ws.repo.clone(),
            worktree,
            scratch: self.paths.tmp.clone(),
            target_branch: st
                .objective()
                .record()
                .integration_target
                .as_str()
                .to_string(),
            capture_id: CaptureId::new(new_uuid()),
            latest: st.capture.as_ref().map(|c| LatestCapture {
                id: c.id,
                tree: c.tree.clone(),
                target: c.target_commit.clone(),
            }),
            drafts,
        })
    }

    pub(crate) fn unsent_references(
        st: &ObjState,
    ) -> Vec<(DraftId, lade_core::domain::inspection::Reference)> {
        st.drafts
            .iter()
            .filter(|d| d.is_unsent())
            .map(|d| (d.id(), d.record().reference.clone()))
            .collect()
    }

    pub(crate) fn request_capture(&mut self, oid: ObjectiveId) {
        let rt = self.rt(oid);
        if rt.capture_running {
            rt.recapture = true;
            return;
        }
        let Some(st) = self.objectives.get(&oid) else {
            return;
        };
        let Some(worktree) = st.row.worktree.clone() else {
            return;
        };
        let Ok(input) = self.capture_input(st, worktree, Self::unsent_references(st)) else {
            return;
        };
        self.rt(oid).capture_running = true;
        self.spawn(move || Completed::Capture {
            oid,
            purpose: CapturePurpose::Idle,
            result: jobs::capture(&input),
        });
    }

    pub(crate) fn register_capture(
        &mut self,
        oid: ObjectiveId,
        out: &CaptureOut,
    ) -> Result<CaptureRow, Rejection> {
        self.obj(oid)?;
        let row = if out.reused {
            sql(storage::load_capture(self.store.conn(), out.capture_id))?
                .filter(|row| row.objective_id == oid)
                .ok_or_else(|| not_found("capture"))?
        } else {
            let row = CaptureRow {
                id: out.capture_id,
                objective_id: oid,
                tree: out.tree.clone(),
                target_commit: out.target.clone(),
                counts: jobs::counts(&out.files),
                retention_ref: format!("refs/lade/retain/capture-{}", out.capture_id),
                created_at: now_ms(),
            };
            sql(storage::insert_capture(self.store.conn(), &row))?;
            row
        };
        let mut changed_drafts = Vec::new();
        let st = self
            .objectives
            .get_mut(&oid)
            .ok_or_else(|| not_found("objective"))?;
        for (id, stale) in &out.stale {
            if st.draft_stale.insert(*id, *stale) != Some(*stale) {
                changed_drafts.push(*id);
            }
        }
        let fresh = !out.reused;
        let counts_changed = st.capture.as_ref().map(|c| c.counts) != Some(row.counts);
        st.capture = Some(row.clone());
        let drafts: Vec<_> = changed_drafts
            .iter()
            .filter_map(|id| st.draft(*id).map(|d| wire_draft(st, d)))
            .collect();
        for draft in drafts {
            self.emit(Event::DraftChanged(draft));
        }
        if fresh {
            self.emit(Event::WorktreeChanged(
                lade_protocol::client::WorktreeChanged {
                    objective_id: wire_id(oid.uuid()),
                    capture_id: wire_id(row.id.uuid()),
                },
            ));
        }
        if fresh || counts_changed || !changed_drafts.is_empty() {
            self.emit_objective(oid);
        }
        Ok(row)
    }

    pub(super) fn capture_done(
        &mut self,
        oid: ObjectiveId,
        purpose: CapturePurpose,
        result: Result<CaptureOut, Rejection>,
    ) {
        let registered =
            result.and_then(|out| self.register_capture(oid, &out).map(|row| (out, row)));
        match purpose {
            CapturePurpose::Idle => {
                let rt = self.rt(oid);
                rt.capture_running = false;
                if std::mem::take(&mut rt.recapture) {
                    self.request_capture(oid);
                }
            }
            CapturePurpose::Changes(ctx) => {
                let reply =
                    registered.and_then(|(out, row)| self.working_change_list(oid, &out, &row));
                self.finish(ctx, reply);
            }
            CapturePurpose::Retry(ctx, request) => match registered {
                Ok(_) => {
                    let _ = self.dispatch(ctx, *request);
                }
                Err(rejection) => self.finish(ctx, Err(rejection)),
            },
            CapturePurpose::Submit(ctx, cmd) => match registered {
                Ok((out, _)) => self.submit_after_capture(ctx, &cmd, &out),
                Err(rejection) => self.finish(ctx, Err(rejection)),
            },
            CapturePurpose::DispatchCheck(input_id) => {
                let stale = registered.map_or(true, |(out, _)| out.stale.iter().any(|(_, s)| *s));
                self.dispatch_checked(oid, input_id, stale);
            }
        }
    }
}

#[cfg(test)]
mod tests;
