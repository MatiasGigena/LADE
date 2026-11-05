//! The controlling client's link: admitting it, request dispatch with request-id and
//! operation-journal deduplication, replies and the event stream.

use std::collections::{HashMap, VecDeque};

use lade_protocol::client::{
    ClientBody, ClientEnvelope, ErrorCode, Event, EventFrame, NoticeCode, NoticeLevel, Outcome,
    Rejection, Reply, ReplyData, ReplyResult, RequestFrame, RequestName, SafeAction, ServiceNotice,
};
use lade_protocol::types::{Counter, Id};
use lade_protocol::{Framing, encode};

use crate::storage::{self, OperationState};
use crate::support::{hex, new_uuid, now_ms};
use crate::transport::{Inbound, Outbound};

use super::convert::{durability, preview, wire_id};
use super::{Flow, OpInfo, ReqCtx, Service, recordable};

pub(super) struct Controller {
    pub(super) conn: u64,
    pub(super) out: Outbound,
    pub(super) request_ids: HashMap<String, [u8; 32]>,
    pub(super) request_order: VecDeque<String>,
}

const REQUEST_ID_MEMORY: usize = 4096;

impl Service {
    pub(super) fn on_link(&mut self, inbound: Inbound) {
        match inbound {
            Inbound::Attach { conn, out } => {
                if self.controller.is_some() {
                    let notice = self.encode_event_frame(
                        0,
                        Event::ServiceNotice(ServiceNotice {
                            level: NoticeLevel::Error,
                            code: NoticeCode::LeaseHeld,
                            message: "another client holds the controlling lease".into(),
                        }),
                    );
                    out.close_with(notice);
                    eprintln!("laded: refused second client conn={conn}: lease_held");
                    return;
                }
                let lease_id = wire_id(new_uuid());
                let welcome = ClientEnvelope::new(ClientBody::Welcome {
                    service_epoch: self.epoch.clone(),
                    lease_id,
                });
                let pushed =
                    encode(&welcome, Framing::Client).is_ok_and(|bytes| out.push(bytes).is_ok());
                if pushed {
                    self.controller = Some(Controller {
                        conn,
                        out,
                        request_ids: HashMap::new(),
                        request_order: VecDeque::new(),
                    });
                    self.announce_dialogs();
                    self.announce_storage(false);
                    for note in std::mem::take(&mut self.recovery_notes) {
                        self.notice(NoticeLevel::Warning, NoticeCode::Recovery, &note);
                    }
                } else {
                    out.close();
                }
            }
            Inbound::Detach { conn } => {
                if self.controller.as_ref().is_some_and(|c| c.conn == conn) {
                    self.controller = None;
                    self.drop_terminals(Some(conn), false);
                }
            }
            Inbound::Invalid { conn, request_id } => {
                let reply = Reply::rejected(
                    request_id,
                    None,
                    Rejection::new(
                        ErrorCode::InvalidRequest,
                        SafeAction::None,
                        "request failed schema validation",
                    ),
                );
                self.send_reply(conn, reply);
            }
            Inbound::Request { conn, frame } => {
                if self.controller.as_ref().is_some_and(|c| c.conn == conn) {
                    self.on_request(conn, frame);
                }
            }
        }
    }

    fn encode_event_frame(&self, sequence: u64, event: Event) -> Option<Vec<u8>> {
        let frame = ClientEnvelope::new(ClientBody::Event(EventFrame {
            service_epoch: self.epoch.clone(),
            sequence: Counter::new(sequence),
            event,
        }));
        encode(&frame, Framing::Client).ok()
    }

    pub(crate) fn emit(&mut self, event: Event) {
        self.sequence += 1;
        if self.controller.is_none() {
            return;
        }
        let name = event.name();
        let Some(bytes) = self.encode_event_frame(self.sequence, event) else {
            eprintln!("laded: event too large to encode: {}", name.as_str());
            return;
        };
        let overflow = self
            .controller
            .as_ref()
            .is_some_and(|c| c.out.push(bytes).is_err());
        if overflow {
            self.drop_controller("outbound queue overflow; re-query snapshot");
        }
    }

    fn drop_controller(&mut self, why: &str) {
        let Some(controller) = self.controller.take() else {
            return;
        };
        self.drop_terminals(Some(controller.conn), false);
        let notice = self.encode_event_frame(
            self.sequence,
            Event::ServiceNotice(ServiceNotice {
                level: NoticeLevel::Warning,
                code: NoticeCode::LinkOverflow,
                message: why.into(),
            }),
        );
        eprintln!(
            "laded: dropped client link conn={}: overflow",
            controller.conn
        );
        controller.out.close_with(notice);
    }

    pub(crate) fn notice(&mut self, level: NoticeLevel, code: NoticeCode, message: &str) {
        self.emit(Event::ServiceNotice(ServiceNotice {
            level,
            code,
            message: preview(message, 512),
        }));
    }

    fn send_reply(&mut self, conn: u64, reply: Reply) {
        let Some(controller) = self.controller.as_ref().filter(|c| c.conn == conn) else {
            return;
        };
        let request_id = reply.request_id.clone();
        let name = reply.result.name();
        let bytes = encode(
            &ClientEnvelope::new(ClientBody::Reply(reply)),
            Framing::Client,
        )
        .or_else(|_| {
            let fallback = Reply::rejected(
                request_id,
                name,
                Rejection::new(
                    ErrorCode::LimitReached,
                    SafeAction::None,
                    "reply exceeds the frame budget",
                ),
            );
            encode(
                &ClientEnvelope::new(ClientBody::Reply(fallback)),
                Framing::Client,
            )
        });
        let Ok(bytes) = bytes else {
            return;
        };
        if controller.out.push(bytes).is_err() {
            self.drop_controller("outbound queue overflow; re-query snapshot");
        }
    }

    fn on_request(&mut self, conn: u64, frame: RequestFrame) {
        let name = frame.request.name();
        let Ok(digest) = frame.canonical_digest() else {
            let reply = Reply::rejected(
                frame.request_id,
                Some(name),
                Rejection::new(
                    ErrorCode::InvalidRequest,
                    SafeAction::None,
                    "request payload cannot be represented as JSON",
                ),
            );
            self.send_reply(conn, reply);
            return;
        };
        let request_key = frame.request_id.as_str().to_owned();
        if let Some(controller) = self.controller.as_mut() {
            match controller.request_ids.get(&request_key) {
                Some(seen) if *seen != digest => {
                    let reply = Reply::rejected(
                        frame.request_id,
                        Some(name),
                        Rejection::new(
                            ErrorCode::DuplicateConflict,
                            SafeAction::None,
                            "request id reused with different content",
                        ),
                    );
                    self.send_reply(conn, reply);
                    return;
                }
                Some(_) => {}
                None => {
                    controller.request_ids.insert(request_key.clone(), digest);
                    controller.request_order.push_back(request_key);
                    if controller.request_order.len() > REQUEST_ID_MEMORY
                        && let Some(old) = controller.request_order.pop_front()
                    {
                        controller.request_ids.remove(&old);
                    }
                }
            }
        }
        let op = frame.operation_id.clone().map(|id| OpInfo {
            id,
            digest: hex(&digest),
        });
        if let Some(op) = &op {
            match storage::load_operation(self.store.conn(), &op.id) {
                Err(_) => {
                    let reply = Reply::rejected(
                        frame.request_id,
                        Some(name),
                        durability("operation journal unavailable"),
                    );
                    self.send_reply(conn, reply);
                    return;
                }
                Ok(Some(row)) => {
                    let reply = Self::duplicate_reply(frame.request_id, name, op, &row);
                    self.send_reply(conn, reply);
                    return;
                }
                Ok(None) => {}
            }
        }
        let ctx = ReqCtx {
            conn,
            request_id: frame.request_id,
            name,
            op: op.clone(),
            recorded: false,
        };
        self.held = Some(Vec::new());
        let flow = self.dispatch(ctx, frame.request);
        let held = self.held.take().unwrap_or_default();
        if let Flow::Deferred = flow
            && let Some(op) = op
        {
            let recorded = storage::record_operation(
                self.store.conn(),
                op.key(name),
                OperationState::Pending,
                None,
                now_ms(),
            );
            if recorded.is_err() {
                eprintln!("laded: could not journal pending operation");
            }
        }
        for (verify, task) in held {
            self.pools.submit(verify, task);
        }
    }

    fn duplicate_reply(
        request_id: Id,
        name: RequestName,
        op: &OpInfo,
        row: &storage::OperationRow,
    ) -> Reply {
        if row.digest != op.digest {
            return Reply::rejected(
                request_id,
                Some(name),
                Rejection::new(
                    ErrorCode::DuplicateConflict,
                    SafeAction::QueryOperation,
                    "operation id reused with different content",
                ),
            );
        }
        match &row.outcome {
            Some(Outcome::Ok(data)) => Reply {
                request_id,
                result: ReplyResult::Ok(data.clone()),
            },
            Some(Outcome::Rejected { name, rejection }) => {
                Reply::rejected(request_id, Some(*name), rejection.clone())
            }
            None => Reply::rejected(
                request_id,
                Some(name),
                Rejection::new(
                    ErrorCode::Conflict,
                    SafeAction::QueryOperation,
                    "operation is still in progress",
                ),
            ),
        }
    }

    pub(crate) fn finish(&mut self, ctx: ReqCtx, result: Result<ReplyData, Rejection>) {
        if let Some(op) = &ctx.op
            && !ctx.recorded
        {
            let outcome = match &result {
                Ok(data) => Some(Outcome::Ok(Box::new(data.clone()))),
                Err(rejection) if recordable(rejection) => Some(Outcome::Rejected {
                    name: ctx.name,
                    rejection: rejection.clone(),
                }),
                Err(_) => None,
            };
            let state = if outcome.is_some() {
                OperationState::Completed
            } else {
                OperationState::Uncertain
            };
            let recorded = storage::record_operation(
                self.store.conn(),
                op.key(ctx.name),
                state,
                outcome.as_ref(),
                now_ms(),
            );
            if recorded.is_err() {
                let reply = Reply::rejected(
                    ctx.request_id,
                    Some(ctx.name),
                    durability("operation outcome could not be recorded"),
                );
                self.send_reply(ctx.conn, reply);
                return;
            }
        }
        let reply = match result {
            Ok(data) => Reply::ok(ctx.request_id, data),
            Err(rejection) => Reply::rejected(ctx.request_id, Some(ctx.name), rejection),
        };
        self.send_reply(ctx.conn, reply);
    }
}
