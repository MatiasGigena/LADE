//! The objective terminal: opening, input, resizing and closing it.

use super::intents::Intent;
use crate::presentation::state::model::ObjectiveId;
use crate::presentation::state::store::Change;
use crate::presentation::terminal::{TerminalSession, TerminalStatus, input_chunks};

use super::Controller;

impl Controller {
    pub fn open_terminal(&self, cols: u16, rows: u16) -> Result<(), String> {
        let id = self.selected_or_err()?;
        let opened = self.store.update(|s| {
            if s.terminal_for(&id)
                .is_some_and(|t| !matches!(t.status, TerminalStatus::Ended(_)))
            {
                return (false, Vec::new());
            }
            s.push_terminal(TerminalSession::opening(
                id.clone(),
                usize::from(cols),
                usize::from(rows),
            ));
            (true, vec![Change::Terminal(id.clone())])
        });
        if opened {
            self.emit(Intent::OpenTerminal {
                objective_id: id,
                cols,
                rows,
            });
        }
        Ok(())
    }

    fn open_terminal_id(&self) -> Option<(ObjectiveId, String)> {
        let state = self.store.state();
        let id = state.ui.selected.clone()?;
        let session = state.terminal_for(&id).filter(|t| t.is_open())?;
        Some((id, session.terminal_id.clone()?))
    }

    pub fn terminal_input(&self, text: &str) -> bool {
        let Some((objective_id, terminal_id)) = self.open_terminal_id() else {
            return false;
        };
        for data in input_chunks(text) {
            self.emit(Intent::TerminalInput {
                objective_id: objective_id.clone(),
                terminal_id: terminal_id.clone(),
                data,
            });
        }
        true
    }

    pub fn resize_terminal(&self, cols: u16, rows: u16) {
        let send = self.store.update(|s| {
            let Some(id) = s.ui.selected.clone() else {
                return (None, Vec::new());
            };
            let Some(session) = s.terminal_for_mut(&id) else {
                return (None, Vec::new());
            };
            let before = session.screen.size();
            session.screen.resize(usize::from(cols), usize::from(rows));
            if session.screen.size() == before {
                return (None, Vec::new());
            }
            let (c, r) = session.screen.size();
            let send = session
                .is_open()
                .then(|| session.terminal_id.clone())
                .flatten()
                .map(|t| {
                    (
                        id.clone(),
                        t,
                        u16::try_from(c).unwrap_or(u16::MAX),
                        u16::try_from(r).unwrap_or(u16::MAX),
                    )
                });
            (send, vec![Change::Terminal(id)])
        });
        if let Some((objective_id, terminal_id, cols, rows)) = send {
            self.emit(Intent::ResizeTerminal {
                objective_id,
                terminal_id,
                cols,
                rows,
            });
        }
    }

    pub fn close_terminal(&self) {
        if let Some((objective_id, terminal_id)) = self.open_terminal_id() {
            self.emit(Intent::CloseTerminal {
                objective_id,
                terminal_id,
            });
        }
    }
}

#[cfg(test)]
mod tests;
