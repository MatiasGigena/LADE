//! Hands transport updates to the GTK main thread through a bounded queue drained by idle
//! callbacks.

use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};

use gtk4::glib;
use gtk4::glib::thread_guard::ThreadGuard;

use super::connection::UpdateSink;
use crate::presentation::state::store::{Store, StoreUpdate};

const DRAIN_BATCH: usize = 512;

/// State shared by every sender clone and the idle callbacks that drain it on the GTK main thread.
struct Shared {
    receiver: Mutex<Receiver<StoreUpdate>>,
    wake_pending: AtomicBool,
    /// The store, which only the thread that called [`connect`] may touch. Always `Some` until drop.
    store: Option<ThreadGuard<Weak<Store>>>,
}

impl Drop for Shared {
    fn drop(&mut self) {
        // The transport thread may hold the last sender. `Weak` is not thread-safe, so a drop there
        // leaks the weak handle instead of touching its count off the main thread.
        if let Some(store) = self.store.take()
            && !store.is_owner()
        {
            std::mem::forget(store);
        }
    }
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

impl UpdateSink for UpdateSender {
    fn deliver(&self, update: StoreUpdate) -> bool {
        self.send(update).is_ok()
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
    if let Some(store) = shared.store.as_ref().and_then(|s| s.get_ref().upgrade()) {
        store.apply_all(updates);
    }
    if more && !shared.wake_pending.swap(true, Ordering::AcqRel) {
        let shared = shared.clone();
        glib::idle_add_once(move || drain(&shared));
    }
}

/// Connects a sender to `store`. Call it on the GTK main thread: the drain callbacks run there
/// and panic if the store is reached from any other thread.
pub fn connect(store: &Rc<Store>, capacity: usize) -> UpdateSender {
    let (sender, receiver) = sync_channel(capacity.max(1));
    UpdateSender {
        sender,
        shared: Arc::new(Shared {
            receiver: Mutex::new(receiver),
            wake_pending: AtomicBool::new(false),
            store: Some(ThreadGuard::new(Rc::downgrade(store))),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::presentation::state::model::{NoticeLevel, NoticeScope, ReportCode, ServiceNotice};
    use crate::presentation::state::store::PresentationState;

    fn notice(n: usize) -> StoreUpdate {
        StoreUpdate::Notice(ServiceNotice {
            level: NoticeLevel::Info,
            code: ReportCode::Recovered,
            message: format!("n{n}"),
            scope: NoticeScope::General,
        })
    }

    #[test]
    fn updates_from_another_thread_reach_the_store_in_order_past_a_full_channel() {
        let context = glib::MainContext::default();
        let store = Store::new(PresentationState::default());
        let sender = connect(&store, 1);
        let producer = std::thread::spawn(move || {
            for n in 0..5 {
                assert_eq!(sender.send(notice(n)), Ok(()));
            }
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        while store.state().notices.len() < 5 {
            assert!(Instant::now() < deadline, "updates were not drained");
            if !context.iteration(false) {
                std::thread::yield_now();
            }
        }
        producer.join().unwrap();
        let messages: Vec<String> = store
            .state()
            .notices
            .iter()
            .map(|n| n.message.clone())
            .collect();
        assert_eq!(messages, ["n0", "n1", "n2", "n3", "n4"]);
    }
}
