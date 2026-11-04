use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};

use gtk4::glib;

use crate::presentation::state::store::{Store, StoreUpdate};

const DRAIN_BATCH: usize = 512;

static NEXT_KEY: AtomicU64 = AtomicU64::new(1);

thread_local! {
    static TARGETS: RefCell<HashMap<u64, Weak<Store>>> = RefCell::new(HashMap::new());
}

struct Shared {
    receiver: Mutex<Receiver<StoreUpdate>>,
    wake_pending: AtomicBool,
    key: u64,
}

#[derive(Clone)]
pub struct UpdateSender {
    sender: SyncSender<StoreUpdate>,
    shared: Arc<Shared>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SendError {
    Closed,
}

impl UpdateSender {
    pub fn send(&self, update: StoreUpdate) -> Result<(), SendError> {
        match self.sender.try_send(update) {
            Ok(()) => {}
            Err(TrySendError::Full(update)) => {
                self.wake();
                self.sender.send(update).map_err(|_| SendError::Closed)?;
            }
            Err(TrySendError::Disconnected(_)) => return Err(SendError::Closed),
        }
        self.wake();
        Ok(())
    }

    fn wake(&self) {
        if !self.shared.wake_pending.swap(true, Ordering::AcqRel) {
            let shared = self.shared.clone();
            glib::idle_add_once(move || drain(&shared));
        }
    }
}

fn drain(shared: &Arc<Shared>) {
    shared.wake_pending.store(false, Ordering::Release);
    let (updates, more) = {
        let Ok(receiver) = shared.receiver.lock() else {
            return;
        };
        let updates: Vec<StoreUpdate> = receiver.try_iter().take(DRAIN_BATCH).collect();
        let more = updates.len() == DRAIN_BATCH;
        (updates, more)
    };
    if updates.is_empty() {
        return;
    }
    let store = TARGETS.with(|t| t.borrow().get(&shared.key).and_then(Weak::upgrade));
    if let Some(store) = store {
        store.apply_all(updates);
    }
    if more && !shared.wake_pending.swap(true, Ordering::AcqRel) {
        let shared = shared.clone();
        glib::idle_add_once(move || drain(&shared));
    }
}

pub fn connect(store: &Rc<Store>, capacity: usize) -> UpdateSender {
    let (sender, receiver) = sync_channel(capacity.max(1));
    let key = NEXT_KEY.fetch_add(1, Ordering::Relaxed);
    TARGETS.with(|t| t.borrow_mut().insert(key, Rc::downgrade(store)));
    UpdateSender {
        sender,
        shared: Arc::new(Shared {
            receiver: Mutex::new(receiver),
            wake_pending: AtomicBool::new(false),
            key,
        }),
    }
}
