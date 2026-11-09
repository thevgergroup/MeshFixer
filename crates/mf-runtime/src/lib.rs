//! Job pool, cancellation and progress reporting for background work
//! (MF-RT-002, MF-RT-003, MF-RT-004).
//!
//! This crate is the foundation for M4's background work: a [`JobPool`]
//! wraps a named `rayon` thread pool, [`CancelToken`]/[`RunContext`] let
//! long-running operations check for user cancellation cheaply and often,
//! and [`Progress`] lets a frontend observe a monotonic stage/fraction
//! without locking.
//!
//! Nothing here introduces nondeterminism: the pool only schedules work
//! that callers already parallelise deterministically (e.g. with
//! deterministic parallel sorts); it does not change *what* is computed.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;

/// A cheaply-clonable flag that background work polls to know whether the
/// user asked for cancellation.
///
/// Cloning is an `Arc` clone (cheap, no allocation beyond the initial one);
/// [`CancelToken::is_cancelled`] is a single atomic load, safe to call in
/// hot loops.
#[derive(Debug, Clone)]
pub struct CancelToken {
    cancelled: Arc<AtomicBool>,
}

impl CancelToken {
    /// Creates a new, not-yet-cancelled token.
    pub fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Requests cancellation. Idempotent.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    /// Returns `true` once [`CancelToken::cancel`] has been called.
    ///
    /// An atomic load: no locks, no allocation. Safe to call every ~32,768
    /// elements (or at least every 100 ms of work) in a hot loop.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

/// The error returned by [`RunContext::checkpoint`] once cancellation has
/// been requested.
#[derive(Debug, thiserror::Error)]
#[error("cancelled by the user")]
pub struct Cancelled;

/// Bundles the pieces a long-running operation needs to cooperate with the
/// rest of the app: a way to notice cancellation and a way to report
/// progress.
#[derive(Debug, Clone)]
pub struct RunContext {
    pub cancel: CancelToken,
    pub progress: Progress,
}

impl RunContext {
    /// A context that never cancels and discards progress. For tests and
    /// call sites that have not yet been threaded through a real pool.
    pub fn none() -> Self {
        Self {
            cancel: CancelToken::new(),
            progress: Progress::new(),
        }
    }

    /// Checks whether cancellation has been requested.
    ///
    /// Cheap: a single atomic load, no allocation. Call this at least every
    /// 100 ms of work; the rule of thumb for tight loops is every 32,768
    /// elements.
    pub fn checkpoint(&self) -> Result<(), Cancelled> {
        if self.cancel.is_cancelled() {
            Err(Cancelled)
        } else {
            Ok(())
        }
    }
}

/// How many threads [`JobPool::new`] chose, and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolChoice {
    pub threads: usize,
    pub cores: usize,
    pub reason: &'static str,
}

/// A named `rayon` thread pool for background work.
///
/// Threads are named `mf-pool-0`, `mf-pool-1`, ... so they are
/// identifiable in a debugger or profiler.
pub struct JobPool {
    pool: rayon::ThreadPool,
}

impl JobPool {
    /// Builds a pool.
    ///
    /// `threads`:
    /// - `None`: default to `max(1, cores - 1)`, leaving one core free for
    ///   the UI/main thread.
    /// - `Some(0)`: clamped up to 1 (a pool needs at least one thread).
    /// - `Some(n)` with `n > cores`: honoured as requested; `reason` notes
    ///   that more threads than cores were requested.
    /// - `Some(n)` otherwise: honoured as requested.
    ///
    /// Panics if the underlying `rayon` pool fails to build (e.g. the OS
    /// refuses to spawn threads); that is an environment failure, not one
    /// this crate should hide behind a fallback.
    pub fn new(threads: Option<usize>) -> (Self, PoolChoice) {
        // `available_parallelism` only errs when the OS cannot answer at
        // all (e.g. a heavily sandboxed environment); 1 core is then the
        // only safe assumption, not a guess hiding a real failure.
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);

        let (chosen, reason): (usize, &'static str) = match threads {
            None => (cores.saturating_sub(1).max(1), "default: all cores but one"),
            Some(0) => (1, "requested 0 threads; using 1"),
            Some(n) if n > cores => (n, "requested more threads than cores"),
            Some(n) => (n, "requested thread count honoured"),
        };

        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(chosen)
            .thread_name(|i| format!("mf-pool-{i}"))
            .build()
            .expect("failed to build mf-runtime job pool");

        let choice = PoolChoice {
            threads: chosen,
            cores,
            reason,
        };

        (Self { pool }, choice)
    }

    /// Runs `f` on this pool's threads and returns its result.
    pub fn install<R>(&self, f: impl FnOnce() -> R + Send) -> R
    where
        R: Send,
    {
        self.pool.install(f)
    }
}

/// Packs a stage index and a fraction (0..=1, scaled to a fixed-point
/// integer) into one `u64` so [`Progress`] can be updated and read with
/// plain atomics: no locks, no allocation, lock-free for readers.
const FRACTION_SCALE: u64 = 1_000_000;

/// A `(stage, fraction)` update, as sent over a [`Progress::channel`].
type ProgressUpdate = (&'static str, f64);

/// The optional fan-out sink a [`Progress`] sends updates to.
type ProgressSink = Arc<std::sync::Mutex<Option<mpsc::Sender<ProgressUpdate>>>>;

fn pack(stage_index: u32, fraction: f64) -> u64 {
    let scaled = (fraction.clamp(0.0, 1.0) * FRACTION_SCALE as f64).round() as u64;
    (u64::from(stage_index) << 32) | scaled.min(FRACTION_SCALE)
}

fn unpack(bits: u64) -> (u32, f64) {
    let stage_index = (bits >> 32) as u32;
    let scaled = bits & 0xFFFF_FFFF;
    (stage_index, scaled as f64 / FRACTION_SCALE as f64)
}

/// Lock-free, monotonic progress reporting for one background job.
///
/// Within a single stage (the same `stage` string reported consecutively),
/// the fraction never decreases, even if a caller reports a smaller value
/// (e.g. a stale update racing a newer one). Starting a new stage resets
/// the monotonic floor. The fraction is always clamped to `0.0..=1.0`.
///
/// `report` and `snapshot` are both plain atomic operations: no locks, so
/// readers (e.g. a UI polling every frame) never block a writer and vice
/// versa.
#[derive(Debug, Clone)]
pub struct Progress {
    // Bit-packed (stage index, fraction) for the atomic fast path.
    state: Arc<AtomicU64>,
    // The current stage's name. Stored separately because `&'static str`
    // does not fit in an atomic; swapping it is guarded by its own lock so
    // a reader never observes a torn write. `report` holds this lock only
    // long enough to compare-and-possibly-swap one pointer, so it never
    // blocks `snapshot` for longer than that.
    stage: Arc<std::sync::RwLock<&'static str>>,
    // Fan-out sink for `channel()` readers, if any is attached. Sending is
    // best-effort: a report must never fail just because no reader is
    // attached, or a reader was dropped.
    sink: ProgressSink,
}

/// A receiver for every `(stage, fraction)` update sent to a [`Progress`],
/// for frontends that want to react to each change rather than poll
/// [`Progress::snapshot`].
#[derive(Debug)]
pub struct ProgressReader {
    receiver: mpsc::Receiver<ProgressUpdate>,
}

impl ProgressReader {
    /// Blocks until the next update, or returns `None` once the
    /// [`Progress`] this reader was created from has been dropped.
    pub fn recv(&self) -> Option<ProgressUpdate> {
        // allow-swallow: RecvError just means "sender dropped" == None here.
        self.receiver.recv().ok()
    }

    /// Returns the next update without blocking, if one is already queued.
    pub fn try_recv(&self) -> Option<ProgressUpdate> {
        // allow-swallow: TryRecvError (empty or disconnected) == None here.
        self.receiver.try_recv().ok()
    }
}

impl Progress {
    /// Creates a fresh tracker at stage `""`, fraction `0.0`.
    ///
    /// Updates have nowhere to go until a reader is attached via
    /// [`Progress::channel`]; `report` and `snapshot` work regardless.
    pub fn new() -> Self {
        Self {
            state: Arc::new(AtomicU64::new(pack(0, 0.0))),
            stage: Arc::new(std::sync::RwLock::new("")),
            sink: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// Reports progress within `stage`. `fraction` is clamped to
    /// `0.0..=1.0` and, within the same stage, never allowed to move
    /// backwards from what was last reported.
    pub fn report(&self, stage: &'static str, fraction: f64) {
        let clamped = fraction.clamp(0.0, 1.0);

        let stage_changed = {
            let current = *self.stage.read().expect("progress stage lock poisoned");
            current != stage
        };

        if stage_changed {
            *self.stage.write().expect("progress stage lock poisoned") = stage;
            // New stage: reset the monotonic floor outright.
            self.state.store(pack(0, clamped), Ordering::Release);
        } else {
            // Same stage: never move the fraction backwards.
            self.state
                .try_update(Ordering::Release, Ordering::Acquire, |bits| {
                    let (idx, current_fraction) = unpack(bits);
                    let next = clamped.max(current_fraction);
                    Some(pack(idx, next))
                })
                .expect("try_update closure always returns Some");
        }

        let (_, reported_fraction) = unpack(self.state.load(Ordering::Acquire));
        let sink = self.sink.lock().expect("progress sink lock poisoned");
        if let Some(sender) = sink.as_ref() {
            // allow-swallow: best-effort fan-out; a dropped reader must not fail reporting.
            let _ = sender.send((stage, reported_fraction));
        }
    }

    /// Reads the current `(stage, fraction)`. Lock-free: a single atomic
    /// load for the fraction and a brief read-lock for the stage name
    /// (readers never block other readers, and never block
    /// [`Progress::report`] for longer than a pointer copy).
    pub fn snapshot(&self) -> (&'static str, f64) {
        let stage = *self.stage.read().expect("progress stage lock poisoned");
        let (_, fraction) = unpack(self.state.load(Ordering::Acquire));
        (stage, fraction)
    }

    /// Attaches a fresh reader to this progress tracker and returns it
    /// alongside a sink clone that feeds it. Every [`Progress::report`]
    /// call on any clone of this tracker (including this one) is sent to
    /// the returned reader from this point on; attaching a new reader
    /// replaces any previous one.
    pub fn channel(&self) -> (Progress, ProgressReader) {
        let (sender, receiver) = mpsc::channel();
        *self.sink.lock().expect("progress sink lock poisoned") = Some(sender);
        (self.clone(), ProgressReader { receiver })
    }
}

impl Default for Progress {
    fn default() -> Self {
        Self::new()
    }
}
