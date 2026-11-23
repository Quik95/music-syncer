//! Progress of a background job that works through a list of entries, such as copying or
//! comparing contents, shared with the UI.

use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};

/// How far a running job got, readable from other threads; also cancels it.
#[derive(Debug, Default)]
pub struct JobProgress {
    bytes: AtomicU64,
    done: AtomicUsize,
    current: Mutex<String>,
    cancelled: AtomicBool,
}

impl JobProgress {
    /// Stops before the next entry; the one being worked on finishes.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    /// Bytes of entries finished so far.
    pub fn bytes(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }

    /// Entries finished so far, failed ones included.
    pub fn done(&self) -> usize {
        self.done.load(Ordering::Relaxed)
    }

    /// Name of the entry being worked on.
    pub fn current(&self) -> String {
        self.current
            .lock()
            .map(|current| current.clone())
            .unwrap_or_default()
    }

    pub fn start(&self, name: String) {
        if let Ok(mut current) = self.current.lock() {
            *current = name;
        }
    }

    pub fn finish(&self, bytes: u64) {
        self.bytes.fetch_add(bytes, Ordering::Relaxed);
        self.done.fetch_add(1, Ordering::Relaxed);
    }
}
