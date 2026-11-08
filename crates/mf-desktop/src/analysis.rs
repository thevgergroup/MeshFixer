//! Background analysis of mesh revisions (diagnostics + render vertices).
//!
//! One worker thread, latest request wins: rapid undo/redo can't pile up
//! threads or memory. Results are cached per revision and identified by
//! pointer, so a result is only ever shown for the revision it describes.

use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use mf_geometry::Mesh;

struct Slot<T> {
    pending: Option<T>,
    shutdown: bool,
}

/// A single background thread that runs only the most recent request.
pub struct LatestWorker<T: Send + 'static> {
    shared: Arc<(Mutex<Slot<T>>, Condvar)>,
    thread: Option<JoinHandle<()>>,
}

impl<T: Send + 'static> LatestWorker<T> {
    pub fn new<R: Send + 'static>(
        work: impl Fn(T) -> R + Send + 'static,
        done: impl Fn(R) + Send + 'static,
    ) -> LatestWorker<T> {
        let shared = Arc::new((
            Mutex::new(Slot {
                pending: None,
                shutdown: false,
            }),
            Condvar::new(),
        ));
        let worker_shared = shared.clone();
        let thread = std::thread::Builder::new()
            .name("mf-analysis".into())
            .spawn(move || {
                let (lock, cvar) = &*worker_shared;
                loop {
                    let request = {
                        let Ok(mut slot) = lock.lock() else { return };
                        loop {
                            if slot.shutdown {
                                return;
                            }
                            if let Some(r) = slot.pending.take() {
                                break r;
                            }
                            slot = match cvar.wait(slot) {
                                Ok(s) => s,
                                Err(_) => return,
                            };
                        }
                    };
                    done(work(request));
                }
            })
            .expect("failed to spawn analysis thread");
        LatestWorker {
            shared,
            thread: Some(thread),
        }
    }

    /// Queue `request`, replacing any request that hasn't started yet.
    pub fn submit(&self, request: T) {
        let (lock, cvar) = &*self.shared;
        if let Ok(mut slot) = lock.lock() {
            slot.pending = Some(request);
            cvar.notify_one();
        }
    }
}

impl<T: Send + 'static> Drop for LatestWorker<T> {
    fn drop(&mut self) {
        let (lock, cvar) = &*self.shared;
        if let Ok(mut slot) = lock.lock() {
            slot.shutdown = true;
            cvar.notify_one();
        }
        // Don't block shutdown on a long analysis; the thread exits after it.
        drop(self.thread.take());
    }
}

/// Small most-recently-inserted cache keyed by mesh revision identity.
pub struct AnalysisCache<V> {
    capacity: usize,
    entries: Vec<(Mesh, V)>,
}

impl<V> AnalysisCache<V> {
    pub fn new(capacity: usize) -> Self {
        AnalysisCache {
            capacity: capacity.max(1),
            entries: Vec::new(),
        }
    }

    pub fn get(&self, mesh: &Mesh) -> Option<&V> {
        self.entries
            .iter()
            .find(|(m, _)| Mesh::ptr_eq(m, mesh))
            .map(|(_, v)| v)
    }

    pub fn insert(&mut self, mesh: Mesh, value: V) {
        self.entries.retain(|(m, _)| !Mesh::ptr_eq(m, &mesh));
        self.entries.push((mesh, value));
        if self.entries.len() > self.capacity {
            self.entries.remove(0);
        }
    }
}
