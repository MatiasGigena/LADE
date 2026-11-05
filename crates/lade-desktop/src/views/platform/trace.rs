use std::cell::{Cell, RefCell};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::presentation::controller::intents::{Intent, IntentSink};

pub const TRACE_ENV: &str = "LADE_DESKTOP_TRACE";

/// The absolute trace path from the environment, read once by `main`.
pub fn path_from_env(env: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    env(TRACE_ENV)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

pub struct Trace {
    file: RefCell<File>,
    paint_pending: Cell<bool>,
}

impl Trace {
    /// Opens the trace file at `path` for appending; `None` when tracing is off or the file is unusable.
    pub fn open(path: Option<&Path>) -> Option<Rc<Self>> {
        let path = path?;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| eprintln!("lade-desktop: trace file unavailable: {}", e.kind()))
            .ok()?;
        Some(Rc::new(Self {
            file: RefCell::new(file),
            paint_pending: Cell::new(false),
        }))
    }

    pub fn event(&self, name: &str, detail: &str) {
        let now = glib::monotonic_time() / 1000;
        let _ = writeln!(self.file.borrow_mut(), "{now} {name} {detail}");
    }

    pub fn changed(&self, name: &str, detail: &str) {
        self.event(name, detail);
        self.paint_pending.set(true);
    }

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
