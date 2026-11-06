//! Contained terminal requests and events.

use super::{TerminalCloseReason, TerminalMode};
use crate::types::{Counter, Id};
use serde::{Deserialize, Serialize};

wire_struct!(TerminalOpenCommand {
    objective_id: Id,
    cols: Counter,
    rows: Counter,
});
wire_struct!(TerminalInfo {
    terminal_id: Id,
    objective_id: Id,
    mode: TerminalMode,
    cols: Counter,
    rows: Counter,
});
wire_struct!(TerminalInputCommand {
    terminal_id: Id,
    data: String,
});
wire_struct!(TerminalResizeCommand {
    terminal_id: Id,
    cols: Counter,
    rows: Counter,
});
wire_struct!(TerminalSelector { terminal_id: Id });
wire_struct!(TerminalOutput {
    objective_id: Id,
    terminal_id: Id,
    seq: Counter,
    data: String,
    dropped_bytes: Counter,
});
wire_struct!(TerminalClosed {
    objective_id: Id,
    terminal_id: Id,
    reason: TerminalCloseReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<Counter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    signal: Option<Counter>,
});
