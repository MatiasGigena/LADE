//! Contained terminals of the controlling client (IMP-012).
//!
//! A terminal is ephemeral interaction, not product state: it is bound to the controlling
//! lease that opened it and to one Objective, never journaled, never recorded in the store and
//! never resumed. Scope: the Objective's worktree is writable only while the Objective is
//! `defined`/`active` and no freeze holds it; a frozen or approved candidate (review writer
//! exclusion) gets a read-only terminal, and writable terminals are killed before a freeze
//! captures. Every terminal dies on lease loss (client closes or is dropped), on a lifecycle
//! that no longer admits it, on `service.stop` and on service shutdown.

use lade_core::domain::ids::ObjectiveId;
use lade_core::domain::objectives::Lifecycle;
use lade_protocol::client::{
    Empty, ErrorCode, Event, Rejection, ReplyData, SafeAction, TerminalCloseReason, TerminalClosed,
    TerminalInfo, TerminalInputCommand, TerminalMode, TerminalOpenCommand, TerminalOutput,
    TerminalResizeCommand, TerminalSelector,
};
use lade_protocol::types::{Counter, Id};

use crate::supervision::terminal::{
    MAX_COLS, MAX_ROWS, TERMINAL_LIMITS, Terminal, TerminalExit, TerminalRequest,
};

use super::convert::{
    conflict, counter, invalid, new_uuid, not_found, parse_uuid, rejection, wire_id,
};
use super::handlers::Pending;
use super::{Flow, JobOutput, ReqCtx, Service};

/// Terminals per Objective and in total (bounded load).
pub(crate) const MAX_PER_OBJECTIVE: usize = 4;
pub(crate) const MAX_TERMINALS: usize = 16;
/// Output bytes per `terminal.output` event, and events per terminal per 100 ms tick.
const EVENT_BYTES: usize = 32 << 10;
const EVENTS_PER_TICK: usize = 2;

/// A live terminal of the controlling client.
pub(crate) struct TermState {
    pub oid: ObjectiveId,
    pub conn: u64,
    pub writable: bool,
    pub handle: Terminal,
    pub seq: u64,
    /// An incomplete UTF-8 sequence at the end of the previous chunk.
    pub tail: Vec<u8>,
}

fn size(value: &Counter, max: u16, what: &str) -> Result<u16, Rejection> {
    u16::try_from(value.value())
        .ok()
        .filter(|v| (1..=max).contains(v))
        .ok_or_else(|| invalid(&format!("terminal {what} must be 1..={max}")))
}

fn unavailable(detail: &str) -> Rejection {
    rejection(ErrorCode::ExecutionUnavailable, SafeAction::None, detail)
}

/// Decodes `bytes` (after a carried incomplete sequence) as UTF-8, replacing invalid bytes and
/// keeping a trailing incomplete sequence for the next chunk. NUL bytes are dropped.
pub(crate) fn decode_chunk(tail: &mut Vec<u8>, bytes: &[u8], last: bool) -> String {
    let mut data = std::mem::take(tail);
    data.extend_from_slice(bytes);
    let mut out = String::with_capacity(data.len());
    let mut rest = &data[..];
    loop {
        match std::str::from_utf8(rest) {
            Ok(text) => {
                out.push_str(text);
                break;
            }
            Err(e) => {
                let (valid, after) = rest.split_at(e.valid_up_to());
                out.push_str(std::str::from_utf8(valid).unwrap_or_default());
                match e.error_len() {
                    Some(bad) => {
                        out.push('\u{fffd}');
                        rest = &after[bad..];
                    }
                    None if last => {
                        out.push('\u{fffd}');
                        break;
                    }
                    None => {
                        tail.extend_from_slice(after);
                        break;
                    }
                }
            }
        }
    }
    out.retain(|c| c != '\0');
    out
}

impl Service {
    /// Whether a terminal may be opened for `oid` now, and whether it may write the worktree.
    fn terminal_scope(&self, oid: ObjectiveId) -> Result<bool, Rejection> {
        let st = self.obj(oid)?;
        let record = st.objective().record();
        let lifecycle_error =
            |detail: &str| rejection(ErrorCode::InvalidLifecycle, SafeAction::Refresh, detail);
        if record.cleaned_up {
            return Err(lifecycle_error(
                "the Objective was cleaned up; it has no worktree",
            ));
        }
        let writable = match st.objective().lifecycle() {
            Lifecycle::Defined | Lifecycle::Active => {
                self.runtime.get(&oid).and_then(|r| r.busy) != Some("freeze")
            }
            Lifecycle::Frozen | Lifecycle::Approved => false,
            _ => {
                return Err(lifecycle_error(
                    "a terminal opens only for an Objective in progress or under review",
                ));
            }
        };
        if !st.row.worktree.as_ref().is_some_and(|w| w.is_dir()) {
            return Err(lifecycle_error(
                "the Objective has no worktree yet; start the agent once to create it",
            ));
        }
        Ok(writable)
    }

    pub(crate) fn terminal_open(&mut self, ctx: ReqCtx, cmd: &TerminalOpenCommand) -> Flow {
        let prepared = (|| {
            let oid = self.objective_id(&cmd.objective_id)?;
            let cols = size(&cmd.cols, MAX_COLS, "columns")?;
            let rows = size(&cmd.rows, MAX_ROWS, "rows")?;
            let writable = self.terminal_scope(oid)?;
            let open = self.terminals.values().filter(|t| t.oid == oid).count();
            if open + self.terminals_opening >= MAX_TERMINALS || open >= MAX_PER_OBJECTIVE {
                return Err(rejection(
                    ErrorCode::LimitReached,
                    SafeAction::None,
                    &format!(
                        "at most {MAX_PER_OBJECTIVE} terminals per Objective and {MAX_TERMINALS} in total"
                    ),
                ));
            }
            let tools = self
                .sandbox_tools()
                .ok_or_else(|| unavailable("terminal sandbox unavailable (bubblewrap/prlimit)"))?;
            let st = self.obj(oid)?;
            let ws = self.ws_of(st)?;
            let terminal_id = new_uuid();
            let request = TerminalRequest {
                objective_id: oid.to_string(),
                terminal_id: terminal_id.to_string(),
                worktree: st.row.worktree.clone().unwrap_or_default(),
                repo_common_dir: ws.repo.common_dir().to_path_buf(),
                writable,
                cols,
                rows,
                run_dir: self.paths.run.clone(),
                limits: TERMINAL_LIMITS,
            };
            Ok((oid, terminal_id, tools, request))
        })();
        let (oid, terminal_id, tools, request) = match prepared {
            Ok(prepared) => prepared,
            Err(rejection) => {
                self.finish(ctx, Err(rejection));
                return Flow::Done;
            }
        };
        self.terminals_opening += 1;
        let writable = request.writable;
        self.spawn(
            Pending::TerminalOpen {
                oid,
                ctx,
                terminal_id: wire_id(terminal_id),
                writable,
                size: (request.cols, request.rows),
            },
            move || {
                JobOutput::Terminal(Terminal::open(&tools, &request).map_err(|e| {
                    eprintln!("laded: terminal could not start: {}", e.kind());
                    unavailable("the terminal sandbox could not start")
                }))
            },
        );
        Flow::Deferred
    }

    pub(crate) fn terminal_open_done(
        &mut self,
        oid: ObjectiveId,
        ctx: ReqCtx,
        opened: (Id, bool, (u16, u16)),
        result: Result<Terminal, Rejection>,
    ) {
        self.terminals_opening = self.terminals_opening.saturating_sub(1);
        let (terminal_id, writable, (cols, rows)) = opened;
        let still_controller = self.controller.as_ref().is_some_and(|c| c.conn == ctx.conn);
        let outcome = result.and_then(|handle| {
            // Revalidate: the lease, the lifecycle and writer exclusion may have changed while the
            // sandbox started.
            let scope = self.terminal_scope(oid);
            let admitted = match scope {
                Ok(now_writable) if still_controller && (now_writable || !writable) => Ok(()),
                Ok(_) => Err(conflict("a review freeze started; open the terminal again")),
                Err(rejection) => Err(rejection),
            };
            match admitted {
                Ok(()) => Ok(handle),
                Err(rejection) => {
                    self.kill_detached(handle);
                    Err(rejection)
                }
            }
        });
        let result = outcome.map(|handle| {
            self.terminals.insert(
                terminal_id.as_str().to_owned(),
                TermState {
                    oid,
                    conn: ctx.conn,
                    writable,
                    handle,
                    seq: 0,
                    tail: Vec::new(),
                },
            );
            ReplyData::TerminalOpen(TerminalInfo {
                terminal_id: terminal_id.clone(),
                objective_id: wire_id(oid.uuid()),
                mode: if writable {
                    TerminalMode::ReadWrite
                } else {
                    TerminalMode::ReadOnly
                },
                cols: counter(u64::from(cols)),
                rows: counter(u64::from(rows)),
            })
        });
        self.finish(ctx, result);
    }

    fn terminal(&self, id: &Id) -> Result<&TermState, Rejection> {
        parse_uuid(id)?;
        self.terminals
            .get(id.as_str())
            .ok_or_else(|| not_found("terminal"))
    }

    pub(crate) fn terminal_input(
        &mut self,
        cmd: &TerminalInputCommand,
    ) -> Result<ReplyData, Rejection> {
        let term = self.terminal(&cmd.terminal_id)?;
        term.handle
            .write(cmd.data.as_bytes())
            .map(|()| ReplyData::TerminalInput(Empty {}))
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::WouldBlock => rejection(
                    ErrorCode::LimitReached,
                    SafeAction::None,
                    "the terminal is not reading input; wait or close it",
                ),
                _ => not_found("terminal"),
            })
    }

    pub(crate) fn terminal_resize(
        &mut self,
        cmd: &TerminalResizeCommand,
    ) -> Result<ReplyData, Rejection> {
        let cols = size(&cmd.cols, MAX_COLS, "columns")?;
        let rows = size(&cmd.rows, MAX_ROWS, "rows")?;
        let term = self.terminal(&cmd.terminal_id)?;
        term.handle
            .resize(cols, rows)
            .map(|()| ReplyData::TerminalResize(Empty {}))
            .map_err(|_| not_found("terminal"))
    }

    pub(crate) fn terminal_close(
        &mut self,
        cmd: &TerminalSelector,
    ) -> Result<ReplyData, Rejection> {
        self.terminal(&cmd.terminal_id)?;
        self.close_terminal(cmd.terminal_id.as_str(), TerminalCloseReason::Closed, false);
        Ok(ReplyData::TerminalClose(Empty {}))
    }

    /// Kills a terminal off the service thread.
    fn kill_detached(&mut self, mut handle: Terminal) {
        self.spawn(Pending::Ignore, move || {
            handle.kill();
            JobOutput::Done
        });
    }

    /// Removes and kills one terminal and reports it closed. `wait` kills on the service thread
    /// (writer exclusion before a freeze captures); otherwise the kill runs on the pool.
    fn close_terminal(&mut self, id: &str, reason: TerminalCloseReason, wait: bool) {
        let Some(mut term) = self.terminals.remove(id) else {
            return;
        };
        let exit = term.handle.exit();
        if wait {
            term.handle.kill();
        } else {
            self.kill_detached(term.handle);
        }
        self.emit_closed(term.oid, id, reason, exit);
    }

    fn emit_closed(
        &mut self,
        oid: ObjectiveId,
        id: &str,
        reason: TerminalCloseReason,
        exit: Option<TerminalExit>,
    ) {
        let Ok(terminal_id) = Id::parse(id.to_owned()) else {
            return;
        };
        let to_counter = |v: Option<i32>| v.and_then(|v| u64::try_from(v).ok()).map(counter);
        self.emit(Event::TerminalClosed(TerminalClosed {
            objective_id: wire_id(oid.uuid()),
            terminal_id,
            reason,
            exit_code: to_counter(exit.and_then(|e| e.code)),
            signal: to_counter(exit.and_then(|e| e.signal)),
        }));
    }

    /// Writer exclusion: kills (and waits for) every writable terminal of `oid` before a freeze
    /// captures the worktree.
    pub(crate) fn exclude_terminal_writers(&mut self, oid: ObjectiveId) {
        let writers: Vec<String> = self
            .terminals
            .iter()
            .filter(|(_, t)| t.oid == oid && t.writable)
            .map(|(id, _)| id.clone())
            .collect();
        for id in writers {
            self.close_terminal(&id, TerminalCloseReason::ReviewFreeze, true);
        }
    }

    /// Closes terminals whose Objective no longer admits them (lifecycle, cleanup, writer
    /// exclusion). Called after every Objective change.
    pub(crate) fn enforce_terminal_scope(&mut self, oid: ObjectiveId) {
        let scope = self.terminal_scope(oid);
        let stale: Vec<String> = self
            .terminals
            .iter()
            .filter(|(_, t)| t.oid == oid && !matches!(scope, Ok(w) if w || !t.writable))
            .map(|(id, _)| id.clone())
            .collect();
        for id in stale {
            let wait = self.terminals.get(&id).is_some_and(|t| t.writable);
            self.close_terminal(&id, TerminalCloseReason::Lifecycle, wait);
        }
    }

    /// Lease loss or shutdown: every terminal of `conn` (or all) dies, without events (there is no
    /// client to receive them).
    pub(crate) fn drop_terminals(&mut self, conn: Option<u64>, wait: bool) {
        let ids: Vec<String> = self
            .terminals
            .iter()
            .filter(|(_, t)| conn.is_none_or(|c| t.conn == c))
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            if let Some(mut term) = self.terminals.remove(&id) {
                if wait {
                    term.handle.kill();
                } else {
                    self.kill_detached(term.handle);
                }
            }
        }
    }

    /// Tick: forwards bounded output and reports terminals that ended.
    pub(crate) fn pump_terminals(&mut self) {
        let ids: Vec<String> = self.terminals.keys().cloned().collect();
        for id in ids {
            for _ in 0..EVENTS_PER_TICK {
                let Some(term) = self.terminals.get_mut(&id) else {
                    break;
                };
                if !term.handle.pending() {
                    break;
                }
                let drained = term.handle.drain(EVENT_BYTES);
                let finished = term.handle.exit().is_some() && !term.handle.pending();
                let data = decode_chunk(&mut term.tail, &drained.bytes, finished);
                term.seq += 1;
                let (oid, seq) = (term.oid, term.seq);
                let Ok(terminal_id) = Id::parse(id.clone()) else {
                    break;
                };
                self.emit(Event::TerminalOutput(TerminalOutput {
                    objective_id: wire_id(oid.uuid()),
                    terminal_id,
                    seq: counter(seq),
                    data,
                    dropped_bytes: counter(drained.dropped),
                }));
            }
            let finished = self
                .terminals
                .get(&id)
                .and_then(|t| t.handle.finished().map(|exit| (t.oid, exit)));
            if let Some((oid, exit)) = finished {
                self.terminals.remove(&id);
                self.emit_closed(oid, &id, TerminalCloseReason::Exited, Some(exit));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::decode_chunk;

    #[test]
    fn chunks_decode_across_split_sequences() {
        let mut tail = Vec::new();
        let text = "añ😀b".as_bytes();
        let first = decode_chunk(&mut tail, &text[..4], false);
        assert_eq!(first, "añ");
        assert_eq!(tail.len(), 1);
        let second = decode_chunk(&mut tail, &text[4..], false);
        assert_eq!(second, "😀b");
        assert!(tail.is_empty());
        assert_eq!(decode_chunk(&mut tail, b"x\xffy\0z", false), "x\u{fffd}yz");
        assert_eq!(decode_chunk(&mut tail, b"\xe2\x82", true), "\u{fffd}");
        assert!(tail.is_empty());
    }
}
