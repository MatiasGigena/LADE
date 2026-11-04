//! The service's job pools: general jobs, verification runs and the reaper.

use std::io;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};

pub(crate) type Task = Box<dyn FnOnce() + Send>;

/// The service's worker threads: general jobs, verification runs (kept apart so long checks
/// never hold up captures and queries), and the reaper that terminates stopped workers.
#[derive(Default)]
pub(crate) struct Pools {
    pub general: Option<Pool>,
    pub verify: Option<Pool>,
    pub reaper: Option<Pool>,
}

const GENERAL_THREADS: usize = 3;
const VERIFY_THREADS: usize = 4;
const REAPER_THREADS: usize = 4;

impl Pools {
    /// Starts the three pools.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error when a pool thread cannot be spawned.
    pub fn start() -> io::Result<Self> {
        Ok(Self {
            general: Some(Pool::new(GENERAL_THREADS)?),
            verify: Some(Pool::new(VERIFY_THREADS)?),
            reaper: Some(Pool::new(REAPER_THREADS)?),
        })
    }

    pub fn submit(&self, verify: bool, task: Task) {
        let pool = if verify { &self.verify } else { &self.general };
        if let Some(pool) = pool {
            pool.submit(task);
        }
    }

    pub fn shutdown(&mut self) {
        for pool in [self.reaper.take(), self.general.take(), self.verify.take()]
            .into_iter()
            .flatten()
        {
            pool.shutdown();
        }
    }
}

pub struct Pool {
    tx: Option<Sender<Task>>,
    threads: Vec<JoinHandle<()>>,
}

impl Pool {
    /// Starts `size` named worker threads that run submitted tasks in order.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error when a thread cannot be spawned; the threads
    /// already started stop once the returned error drops the task channel.
    pub fn new(size: usize) -> io::Result<Self> {
        let (tx, rx) = mpsc::channel::<Task>();
        let rx = Arc::new(Mutex::new(rx));
        let threads = (0..size)
            .map(|index| {
                let rx: Arc<Mutex<Receiver<Task>>> = Arc::clone(&rx);
                thread::Builder::new()
                    .name(format!("laded-pool-{index}"))
                    .spawn(move || {
                        loop {
                            let task = rx.lock().unwrap_or_else(PoisonError::into_inner).recv();
                            match task {
                                Ok(task) => task(),
                                Err(_) => break,
                            }
                        }
                    })
            })
            .collect::<io::Result<_>>()?;
        Ok(Self {
            tx: Some(tx),
            threads,
        })
    }

    pub fn submit(&self, task: impl FnOnce() + Send + 'static) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Box::new(task));
        }
    }

    pub fn shutdown(mut self) {
        self.tx = None;
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}
