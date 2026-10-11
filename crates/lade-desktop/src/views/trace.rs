//! Opt-in presentation timing trace for the native reference measurements (`scripts/e2e-perf`).
//!
//! Inert unless the window was started with `LADE_DESKTOP_TRACE` naming an absolute file. Each
//! line is `<monotonic ms> <event> <detail>`: `intent` when a request leaves the views (detail:
//! the intent label), `draft-edit` when a draft's text changes, semantic store changes (`draft`,
//! `viewer`, `activity`, `detail`), and `paint` after the first frame painted following such a
//! change. Details carry identifiers, labels, states and sizes only — never file content, draft
//! or message text. "Painted" is GTK's after-paint of the window's frame clock: the frame was
//! rendered by the toolkit, not confirmed on screen by the compositor.

use std::cell::{Cell, RefCell};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::presentation::intents::{Intent, IntentSink};

pub const TRACE_ENV: &str = "LADE_DESKTOP_TRACE";

pub struct Trace {
    file: RefCell<File>,
    paint_pending: Cell<bool>,
}

impl Trace {
    /// The trace named by [`TRACE_ENV`], if set to an absolute path that can be opened.
    pub fn from_env() -> Option<Rc<Self>> {
        let path = std::env::var_os(TRACE_ENV)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())?;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| eprintln!("lade-desktop: trace file unavailable: {}", e.kind()))
            .ok()?;
        Some(Rc::new(Self {
            file: RefCell::new(file),
            paint_pending: Cell::new(false),
        }))
    }

    pub fn event(&self, name: &str, detail: &str) {
        let now = glib::monotonic_time() / 1000;
        // Best effort: a full disk must not disturb the window.
        let _ = writeln!(self.file.borrow_mut(), "{now} {name} {detail}");
    }

    /// Records a semantic change and asks for the next `paint` line.
    pub fn changed(&self, name: &str, detail: &str) {
        self.event(name, detail);
        self.paint_pending.set(true);
    }

    /// Writes `paint` after every frame that follows a recorded change.
    pub fn watch_paints(self: &Rc<Self>, window: &gtk::ApplicationWindow) {
        let trace = self.clone();
        window.connect_realize(move |window| {
            let Some(clock) = window.frame_clock() else {
                return;
            };
            let trace = trace.clone();
            clock.connect_after_paint(move |_| {
                if trace.paint_pending.replace(false) {
                    trace.event("paint", "");
                }
            });
        });
    }
}

/// Records each intent's label, then forwards it.
pub struct TracingSink {
    pub inner: Rc<dyn IntentSink>,
    pub trace: Rc<Trace>,
}

impl IntentSink for TracingSink {
    fn submit(&self, intent: Intent) {
        self.trace.event("intent", intent.label());
        self.inner.submit(intent);
    }
}
