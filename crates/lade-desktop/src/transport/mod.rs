//! Client wiring to the authoritative `laded` service (WP-G).
//!
//! The GTK main context only enqueues intents ([`Transport`] is the views' `IntentSink`); a
//! dedicated driver thread owns the Unix-socket link, a reader thread decodes frames, and every
//! result reaches the presentation store as a `StoreUpdate` through the bounded `UpdateSender`.

pub mod connection;
pub mod convert;
pub mod core;
pub mod requests;

use std::cell::RefCell;
use std::sync::mpsc::{Receiver, Sender, channel};

use connection::{Config, Input, UpdateSink};

use crate::presentation::intents::{Intent, IntentSink};
use crate::presentation::store::StoreUpdate;
use crate::views::bridge::UpdateSender;

impl UpdateSink for UpdateSender {
    fn deliver(&self, update: StoreUpdate) -> bool {
        self.send(update).is_ok()
    }
}

/// Main-context handle: never blocks, only forwards intents to the driver thread.
pub struct Transport {
    tx: Sender<Input>,
    rx: RefCell<Option<Receiver<Input>>>,
}

impl Transport {
    /// Intents submitted before [`Transport::start`] are kept and handled once it runs.
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Self {
            tx,
            rx: RefCell::new(Some(rx)),
        }
    }

    /// Starts the driver thread; later calls do nothing.
    pub fn start(&self, config: Config, sink: impl UpdateSink) -> std::io::Result<()> {
        let Some(rx) = self.rx.borrow_mut().take() else {
            return Ok(());
        };
        connection::spawn_driver(config, sink, self.tx.clone(), rx).map(drop)
    }
}

impl IntentSink for Transport {
    fn submit(&self, intent: Intent) {
        // The driver only exits once the window is gone; a failed send has no one to tell.
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
