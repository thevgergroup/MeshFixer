//! Background analysis of mesh revisions (diagnostics + render vertices).
//!
//! Latest request wins: a new request cancels the one running, and at most
//! one runs at a time (on the app's job pool), so rapid undo/redo can't
//! pile up work or memory. Results are cached per revision and identified
//! by pointer, so a result is only ever shown for the revision it
//! describes.

use std::sync::{Arc, Mutex};

use mf_geometry::Mesh;
use mf_runtime::{CancelToken, Cancelled, JobPool, RunContext};

struct Slot<T> {
    pending: Option<(T, RunContext)>,
    /// A drain loop is on the pool.
    running: bool,
    /// The newest request's token, cancelled when it is superseded.
    latest: Option<CancelToken>,
    shutdown: bool,
}

/// Runs one request with its context, reporting only finished work.
type Run<T> = Arc<dyn Fn(T, &RunContext) + Send + Sync>;

/// Runs only the most recent request, on a shared pool.
pub struct LatestWorker<T: Send + 'static> {
    slot: Arc<Mutex<Slot<T>>>,
    pool: Arc<JobPool>,
    run: Run<T>,
}

impl<T: Send + 'static> LatestWorker<T> {
    /// `work` returns `Err(Cancelled)` once a newer request has superseded
    /// it; only finished work reaches `done`.
    pub fn new<R: 'static>(
        pool: Arc<JobPool>,
        work: impl Fn(T, &RunContext) -> Result<R, Cancelled> + Send + Sync + 'static,
        done: impl Fn(R) + Send + Sync + 'static,
    ) -> LatestWorker<T> {
        LatestWorker {
            slot: Arc::new(Mutex::new(Slot {
                pending: None,
                running: false,
                latest: None,
                shutdown: false,
            })),
            pool,
            run: Arc::new(move |request, ctx| {
                // Err is a superseded request: there's nothing to report.
                if let Ok(result) = work(request, ctx) {
                    done(result);
                }
            }),
        }
    }

    /// Queue `request`, cancelling the one running and replacing any that
    /// hasn't started yet.
    pub fn submit(&self, request: T) {
        let mut slot = self.slot.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(old) = slot.latest.take() {
            old.cancel();
        }
        let ctx = RunContext::none();
        slot.latest = Some(ctx.cancel.clone());
        slot.pending = Some((request, ctx));
        if slot.running {
            return;
        }
        slot.running = true;
        let (shared, run) = (self.slot.clone(), self.run.clone());
        self.pool.spawn(move || {
            loop {
                let (request, ctx) = {
                    let mut slot = shared.lock().unwrap_or_else(|p| p.into_inner());
                    match slot.pending.take() {
                        Some(next) if !slot.shutdown => next,
                        _ => {
                            slot.running = false;
                            return;
                        }
                    }
                };
                run(request, &ctx);
            }
        });
    }
}

impl<T: Send + 'static> Drop for LatestWorker<T> {
    fn drop(&mut self) {
        let mut slot = self.slot.lock().unwrap_or_else(|p| p.into_inner());
        slot.shutdown = true;
        slot.pending = None;
        // Don't block shutdown on a long analysis: cancel it.
        if let Some(t) = slot.latest.take() {
            t.cancel();
        }
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
