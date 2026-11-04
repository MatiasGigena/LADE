//! The bounded queue that carries bytes from the service thread to a sandbox's input (a
//! worker's stdin, a terminal's pty), and the writer thread that drains it.

use std::io::{self, ErrorKind, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::thread;

pub(crate) struct InputQueue {
    tx: Option<SyncSender<Vec<u8>>>,
    queued: Arc<AtomicUsize>,
    failed: Arc<AtomicBool>,
    max_bytes: usize,
    full: &'static str,
}

impl InputQueue {
    /// Starts the thread `name` that writes each queued chunk to `out`, flushing after each, and
    /// stops at the first failed write. At most `frames` chunks and `max_bytes` bytes wait at
    /// once; `full` is the reason a push past either bound reports.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error when the thread cannot be spawned.
    pub(crate) fn start(
        name: &str,
        mut out: impl Write + Send + 'static,
        frames: usize,
        max_bytes: usize,
        full: &'static str,
    ) -> io::Result<Self> {
        let (tx, rx) = sync_channel::<Vec<u8>>(frames);
        let queued = Arc::new(AtomicUsize::new(0));
        let failed = Arc::new(AtomicBool::new(false));
        let (released, broken) = (Arc::clone(&queued), Arc::clone(&failed));
        thread::Builder::new().name(name.into()).spawn(move || {
            for chunk in rx {
                let written = out.write_all(&chunk).and_then(|()| out.flush());
                released.fetch_sub(chunk.len(), Ordering::SeqCst);
                if written.is_err() {
                    broken.store(true, Ordering::SeqCst);
                    break;
                }
            }
        })?;
        Ok(Self {
            tx: Some(tx),
            queued,
            failed,
            max_bytes,
            full,
        })
    }

    /// Whether a write to the input failed, which ends the link.
    pub(crate) fn failed(&self) -> bool {
        self.failed.load(Ordering::SeqCst)
    }

    /// Queues `chunk` for the writer.
    ///
    /// # Errors
    ///
    /// Returns `gone()` when the queue was closed or its writer stopped, or `WouldBlock` with the
    /// `full` reason when the chunk would exceed the frame or byte bound.
    pub(crate) fn push(&self, chunk: Vec<u8>, gone: impl Fn() -> io::Error) -> io::Result<()> {
        let Some(tx) = self.tx.as_ref() else {
            return Err(gone());
        };
        let len = chunk.len();
        let full = || io::Error::new(ErrorKind::WouldBlock, self.full);
        if self.queued.fetch_add(len, Ordering::SeqCst) + len > self.max_bytes {
            self.queued.fetch_sub(len, Ordering::SeqCst);
            return Err(full());
        }
        match tx.try_send(chunk) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.queued.fetch_sub(len, Ordering::SeqCst);
                Err(match error {
                    TrySendError::Full(_) => full(),
                    TrySendError::Disconnected(_) => gone(),
                })
            }
        }
    }

    /// Closes the queue; the writer finishes the queued chunks and exits.
    pub(crate) fn close(&mut self) {
        self.tx = None;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, PoisonError, mpsc};
    use std::time::Duration;

    use super::*;

    fn gone() -> io::Error {
        io::Error::new(ErrorKind::BrokenPipe, "gone")
    }

    /// Records what it is given, waiting for a go-ahead before each write when `gated`.
    struct Sink {
        written: Arc<Mutex<Vec<u8>>>,
        gate: Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>,
        fail: bool,
    }

    impl Write for Sink {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if let Some((entered, gate)) = &self.gate {
                let _ = entered.send(());
                let _ = gate.recv();
            }
            if self.fail {
                return Err(io::Error::other("disk full"));
            }
            self.written
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn wait_for(condition: impl Fn() -> bool) {
        for _ in 0..500 {
            if condition() {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("condition not reached");
    }

    #[test]
    fn chunks_reach_the_writer_in_order_and_release_their_bytes() {
        let written = Arc::new(Mutex::new(Vec::new()));
        let sink = Sink {
            written: Arc::clone(&written),
            gate: None,
            fail: false,
        };
        let mut queue = InputQueue::start("test-input", sink, 8, 64, "full").expect("start");
        queue.push(b"ab".to_vec(), gone).expect("push");
        queue.push(b"cd".to_vec(), gone).expect("push");
        wait_for(|| written.lock().expect("lock").len() == 4);
        assert_eq!(*written.lock().expect("lock"), b"abcd");
        wait_for(|| queue.queued.load(Ordering::SeqCst) == 0);
        queue.close();
        let error = queue.push(b"late".to_vec(), gone).expect_err("closed");
        assert_eq!(error.kind(), ErrorKind::BrokenPipe);
    }

    #[test]
    fn pushes_past_the_byte_or_frame_bound_are_refused_until_the_writer_catches_up() {
        let (go, gate) = mpsc::channel();
        let (entered_tx, entered) = mpsc::channel();
        let sink = Sink {
            written: Arc::default(),
            gate: Some((entered_tx, gate)),
            fail: false,
        };
        let queue = InputQueue::start("test-bounds", sink, 2, 10, "queue_full").expect("start");
        queue.push(vec![0; 6], gone).expect("first");
        entered.recv().expect("the writer holds the first chunk");
        let over = queue
            .push(vec![0; 5], gone)
            .expect_err("over the byte bound");
        assert_eq!(over.kind(), ErrorKind::WouldBlock);
        assert_eq!(over.to_string(), "queue_full");
        assert_eq!(
            queue.queued.load(Ordering::SeqCst),
            6,
            "a refused push releases its bytes"
        );
        queue.push(vec![0; 1], gone).expect("second");
        queue
            .push(vec![0; 1], gone)
            .expect("third fills the channel");
        let crowded = queue
            .push(vec![0; 1], gone)
            .expect_err("over the frame bound");
        assert_eq!(crowded.kind(), ErrorKind::WouldBlock);
        for _ in 0..3 {
            go.send(()).expect("go");
        }
        wait_for(|| queue.queued.load(Ordering::SeqCst) == 0);
        queue.push(vec![0; 10], gone).expect("room again");
        drop(go);
    }

    #[test]
    fn a_failed_write_ends_the_link() {
        let sink = Sink {
            written: Arc::default(),
            gate: None,
            fail: true,
        };
        let queue = InputQueue::start("test-failure", sink, 4, 64, "full").expect("start");
        assert!(!queue.failed());
        queue.push(b"x".to_vec(), gone).expect("queued");
        wait_for(|| queue.failed());
        wait_for(|| {
            queue
                .push(b"y".to_vec(), gone)
                .is_err_and(|e| e.kind() == ErrorKind::BrokenPipe)
        });
    }
}
