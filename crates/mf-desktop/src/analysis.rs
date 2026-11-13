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

/// What GPU memory a cached value holds: each allocation as a key and
/// its size in bytes. An allocation shared between values (a GPU chunk two
/// revisions share, MF-RENDER-003) has the same key in each.
pub trait GpuHeld {
    fn gpu_held(&self, out: &mut Vec<(usize, u64)>);
}

/// GPU memory the cached revisions may hold beyond what the current
/// revisions (those the view draws) hold, by default (M4 final review I5).
pub const DEFAULT_EXTRA_GPU_BYTES: u64 = 1 << 30;

/// At most this many revisions are cached, whatever their GPU size: each
/// analysis also holds CPU memory (defects, overlays).
pub const MAX_CACHED_REVISIONS: usize = 16;

/// Analyses keyed by mesh revision identity, bounded by the GPU memory
/// they hold beyond the current revisions' (see [`AnalysisCache::insert`]),
/// least recently used out first. A count bound alone was not enough: one
/// 30M-triangle revision's surface is ~2 GB, and a whole-mesh edit (compact,
/// remove loose pieces) shares none of it with its parent.
pub struct AnalysisCache<V> {
    extra_gpu_bytes: u64,
    /// Least recently used first.
    entries: Vec<(Mesh, V)>,
}

impl<V: GpuHeld> AnalysisCache<V> {
    /// A cache whose non-current revisions hold at most `extra_gpu_bytes`
    /// of GPU memory the current ones don't.
    pub fn new(extra_gpu_bytes: u64) -> Self {
        AnalysisCache {
            extra_gpu_bytes,
            entries: Vec::new(),
        }
    }

    pub fn get(&self, mesh: &Mesh) -> Option<&V> {
        self.entries
            .iter()
            .find(|(m, _)| Mesh::ptr_eq(m, mesh))
            .map(|(_, v)| v)
    }

    /// Revisions cached.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Mark `mesh`'s entry as the most recently used. O(entries).
    pub fn touch(&mut self, mesh: &Mesh) {
        if let Some(i) = self.entries.iter().position(|(m, _)| Mesh::ptr_eq(m, mesh))
            && i + 1 < self.entries.len()
        {
            let entry = self.entries.remove(i);
            self.entries.push(entry);
        }
    }

    /// Add (or replace) `mesh`'s entry as the most recently used, then make
    /// room: least recently used first, entries whose mesh isn't `current`
    /// go until the rest hold at most the budget of GPU memory the current
    /// ones don't, and at most [`MAX_CACHED_REVISIONS`] entries are left.
    /// Current revisions (what the view draws) are never evicted. Returns
    /// what was replaced or evicted, so the caller can free it where
    /// that's cheap. O(entries × chunks).
    pub fn insert(
        &mut self,
        mesh: Mesh,
        value: V,
        current: impl Fn(&Mesh) -> bool,
    ) -> Vec<(Mesh, V)> {
        let mut out: Vec<(Mesh, V)> = self
            .entries
            .iter()
            .position(|(m, _)| Mesh::ptr_eq(m, &mesh))
            .map(|i| self.entries.remove(i))
            .into_iter()
            .collect();
        self.entries.push((mesh, value));
        while self.entries.len() > MAX_CACHED_REVISIONS
            || self.extra_gpu_bytes(&current) > self.extra_gpu_bytes
        {
            let Some(i) = self.entries.iter().position(|(m, _)| !current(m)) else {
                break;
            };
            out.push(self.entries.remove(i));
        }
        out
    }

    /// GPU bytes held by non-`current` entries and by no `current` one,
    /// each shared allocation counted once: what evicting every
    /// non-current entry would free.
    pub fn extra_gpu_bytes(&self, current: impl Fn(&Mesh) -> bool) -> u64 {
        let mut held = Vec::new();
        let mut pinned = std::collections::HashSet::new();
        for (m, v) in &self.entries {
            if current(m) {
                held.clear();
                v.gpu_held(&mut held);
                pinned.extend(held.iter().map(|(key, _)| *key));
            }
        }
        let mut seen = std::collections::HashSet::new();
        let mut extra = 0;
        for (m, v) in &self.entries {
            if current(m) {
                continue;
            }
            held.clear();
            v.gpu_held(&mut held);
            for (key, bytes) in &held {
                if !pinned.contains(key) && seen.insert(*key) {
                    extra += bytes;
                }
            }
        }
        extra
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // tests may panic on failure
mod tests {
    use super::*;

    /// A cached value holding GPU allocations `(key, bytes)`; equal keys
    /// are one allocation shared between values.
    struct Held(Vec<(usize, u64)>);

    impl GpuHeld for Held {
        fn gpu_held(&self, out: &mut Vec<(usize, u64)>) {
            out.extend_from_slice(&self.0);
        }
    }

    fn revision() -> Mesh {
        mf_fixtures::cube(1.0)
    }

    fn cached(cache: &AnalysisCache<Held>, meshes: &[&Mesh]) -> Vec<bool> {
        meshes.iter().map(|m| cache.get(m).is_some()).collect()
    }

    /// M4 final review I5: cached revisions are bounded by the GPU memory
    /// they hold beyond what the current revisions hold, least recently
    /// used out first; a 30M-triangle revision is ~2 GB, so a count bound
    /// could hold several.
    #[test]
    fn revisions_beyond_the_extra_gpu_budget_go_least_recently_used_first() {
        let mut cache = AnalysisCache::new(100);
        let (current, b, c, d) = (revision(), revision(), revision(), revision());
        let pinned = |m: &Mesh| Mesh::ptr_eq(m, &current);
        // The current revision is kept whatever it holds.
        assert!(
            cache
                .insert(current.clone(), Held(vec![(1, 1000)]), pinned)
                .is_empty()
        );
        assert_eq!(cache.extra_gpu_bytes(pinned), 0);
        assert!(
            cache
                .insert(b.clone(), Held(vec![(2, 60)]), pinned)
                .is_empty()
        );
        assert_eq!(cache.extra_gpu_bytes(pinned), 60);
        // 120 bytes extra: the least recently used (b) goes.
        let evicted = cache.insert(c.clone(), Held(vec![(3, 60)]), pinned);
        assert_eq!(evicted.len(), 1);
        assert!(Mesh::ptr_eq(&evicted[0].0, &b));
        assert_eq!(cached(&cache, &[&current, &b, &c]), [true, false, true]);
        assert_eq!(cache.extra_gpu_bytes(pinned), 60);

        // Using a revision makes it the most recently used: c, used after
        // b was put back, outlives b.
        assert!(
            cache
                .insert(b.clone(), Held(vec![(2, 30)]), pinned)
                .is_empty()
        );
        cache.touch(&c);
        let evicted = cache.insert(d.clone(), Held(vec![(4, 30)]), pinned);
        assert_eq!(evicted.len(), 1);
        assert!(Mesh::ptr_eq(&evicted[0].0, &b));
        assert_eq!(
            cached(&cache, &[&current, &b, &c, &d]),
            [true, false, true, true]
        );
        assert!(cache.extra_gpu_bytes(pinned) <= 100);
    }

    /// Bytes shared with a current revision cost nothing, and bytes shared
    /// between cached revisions count once: they are freed only with the
    /// last revision holding them.
    #[test]
    fn shared_gpu_chunks_count_once_and_not_at_all_when_current() {
        let mut cache = AnalysisCache::new(100);
        let (current, b, c) = (revision(), revision(), revision());
        let pinned = |m: &Mesh| Mesh::ptr_eq(m, &current);
        cache.insert(current.clone(), Held(vec![(1, 1000), (9, 10)]), pinned);
        // b and c share chunk 1 with the current revision, and chunk 2
        // with each other.
        cache.insert(b.clone(), Held(vec![(1, 1000), (2, 80)]), pinned);
        let evicted = cache.insert(c.clone(), Held(vec![(1, 1000), (2, 80), (3, 15)]), pinned);
        assert!(evicted.is_empty());
        assert_eq!(cache.extra_gpu_bytes(pinned), 95);
        assert_eq!(cached(&cache, &[&current, &b, &c]), [true, true, true]);
    }

    /// Current revisions (one per object, all drawn) are never evicted,
    /// even over the budget; only the others make room.
    #[test]
    fn current_revisions_are_never_evicted() {
        let mut cache = AnalysisCache::new(0);
        let (one, two, old) = (revision(), revision(), revision());
        let pinned = |m: &Mesh| Mesh::ptr_eq(m, &one) || Mesh::ptr_eq(m, &two);
        // `old` was current when it came.
        cache.insert(old.clone(), Held(vec![(3, 5)]), |_| true);
        let evicted = cache.insert(one.clone(), Held(vec![(1, 500)]), pinned);
        assert_eq!(evicted.len(), 1);
        assert!(Mesh::ptr_eq(&evicted[0].0, &old));
        assert!(
            cache
                .insert(two.clone(), Held(vec![(2, 500)]), pinned)
                .is_empty()
        );
        assert_eq!(cached(&cache, &[&one, &two, &old]), [true, true, false]);
        assert_eq!(cache.extra_gpu_bytes(pinned), 0);
    }

    /// GPU-free revisions (tiny meshes) are still bounded in number: their
    /// analyses hold CPU memory too.
    #[test]
    fn revisions_are_bounded_in_number_too() {
        let mut cache = AnalysisCache::new(u64::MAX);
        let meshes: Vec<Mesh> = (0..MAX_CACHED_REVISIONS + 5).map(|_| revision()).collect();
        for m in &meshes {
            cache.insert(m.clone(), Held(vec![]), |_| false);
        }
        let kept = meshes.iter().filter(|m| cache.get(m).is_some()).count();
        assert_eq!(kept, MAX_CACHED_REVISIONS);
        assert!(
            cache.get(meshes.last().unwrap()).is_some(),
            "the newest stays"
        );
    }
}
