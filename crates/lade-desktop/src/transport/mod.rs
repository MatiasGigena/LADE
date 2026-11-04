pub mod agent_session;
pub mod bridge;
pub mod connection;
pub mod convert;
pub mod core;
pub mod requests;

use std::cell::RefCell;
use std::sync::mpsc::{Receiver, Sender, channel};

use connection::{Config, Input, UpdateSink};

use crate::presentation::controller::intents::{Intent, IntentSink};

pub struct Transport {
    tx: Sender<Input>,
    rx: RefCell<Option<Receiver<Input>>>,
}

impl Transport {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Self {
            tx,
            rx: RefCell::new(Some(rx)),
        }
    }

    pub fn start(&self, config: Config, sink: impl UpdateSink) -> std::io::Result<()> {
        let Some(rx) = self.rx.borrow_mut().take() else {
            return Ok(());
        };
        connection::spawn_driver(config, sink, self.tx.clone(), rx).map(drop)
    }
}

impl IntentSink for Transport {
    fn submit(&self, intent: Intent) {
        let _ = self.tx.send(Input::Intent(intent));
    }
}

impl Drop for Transport {
    fn drop(&mut self) {
        let _ = self.tx.send(Input::Shutdown);
    }
}

impl Default for Transport {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
