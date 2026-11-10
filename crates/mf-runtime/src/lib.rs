//! Job pool, cancellation and progress reporting for background work
//! (MF-RT-002, MF-RT-003, MF-RT-004).
//!
//! This crate is the foundation for M4's background work: a [`JobPool`]
//! wraps a named `rayon` thread pool, [`CancelToken`]/[`RunContext`] let
//! long-running operations check for user cancellation cheaply and often
//! (a single atomic load, no locks), and [`Progress`] lets a frontend
//! observe a monotonic `(stage, fraction)` pair that is always internally
//! consistent — reading never blocks writing or vice versa, and the pair
//! is never torn — via one atomic word holding both stage identity and
//! fraction together (a private stage-name table is the one place a lock
//! is used, and only for the cold path of a never-seen-before stage name).
//!
//! Nothing here introduces nondeterminism: the pool only schedules work
//! that callers already parallelise deterministically (e.g. with
//! deterministic parallel sorts); it does not change *what* is computed.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;

mod sort;
pub use sort::par_sort_unstable_by_key;

/// A cheaply-clonable flag that background work polls to know whether the
/// user asked for cancellation.
///
/// Cloning is an `Arc` clone (cheap, no allocation beyond the initial one);
/// [`CancelToken::is_cancelled`] is a single atomic load (plus a branch on
/// whether a test probe is attached), safe to call in hot loops.
#[derive(Debug, Clone)]
pub struct CancelToken {
    cancelled: Arc<AtomicBool>,
    /// Test instrumentation (see [`RunContext::instrumented`]); `None` in
    /// production, so checking costs one predictable branch.
    probe: Option<Arc<ProbeState>>,
    /// Test instrumentation (see [`RunContext::cancelled_after`]): checks
    /// left before the token cancels itself. `None` in production.
    countdown: Option<Arc<AtomicU64>>,
}

impl CancelToken {
    /// Creates a new, not-yet-cancelled token.
    pub fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            probe: None,
            countdown: None,
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
    #[inline]
    #[track_caller]
    pub fn is_cancelled(&self) -> bool {
        if let Some(probe) = &self.probe {
            probe.hit(std::panic::Location::caller());
        }
        if let Some(left) = &self.countdown
            && left
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1))
                .is_err()
        {
            self.cancelled.store(true, Ordering::Relaxed);
        }
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
    #[inline]
    #[track_caller]
    pub fn checkpoint(&self) -> Result<(), Cancelled> {
        if self.cancel.is_cancelled() {
            Err(Cancelled)
        } else {
            Ok(())
        }
    }

    /// A counter for hot loops: [`Ticker::tick`] checkpoints once every
    /// [`TICK_INTERVAL`] calls, so a loop body stays a counter increment.
    pub fn ticker(&self) -> Ticker<'_> {
        Ticker { ctx: self, n: 0 }
    }

    /// `total` units of work reported as `from..=to` of `stage`: see
    /// [`Span::at`]. Reports `from` now.
    pub fn span(&self, stage: &'static str, from: f64, to: f64, total: usize) -> Span<'_> {
        self.progress.report(stage, from);
        Span {
            ctx: self,
            stage,
            from,
            to,
            total: total.max(1),
        }
    }

    /// A context that cancels itself at its `n + 1`th cancellation check
    /// (the first `n` pass), counted over every clone and thread. With the
    /// number of checks a whole run makes (from
    /// [`RunContext::instrumented`]'s [`CheckpointProbe::checkpoints`]),
    /// a test can cancel a run at every checkpoint in turn (Review Focus
    /// 3: cancel at any stage changes nothing).
    pub fn cancelled_after(n: u64) -> RunContext {
        let mut cancel = CancelToken::new();
        cancel.countdown = Some(Arc::new(AtomicU64::new(n)));
        RunContext {
            cancel,
            progress: Progress::new(),
        }
    }

    /// A context whose checkpoints are timed, for tests that check work
    /// polls for cancellation often enough (MF-RT-002: every 100 ms).
    /// The probe sees every checkpoint made through this context or any
    /// clone of it, from any thread.
    pub fn instrumented() -> (RunContext, CheckpointProbe) {
        let state = Arc::new(ProbeState {
            inner: std::sync::Mutex::new(ProbeInner {
                last: std::time::Instant::now(),
                last_at: None,
                worst: None,
                count: 0,
            }),
        });
        let mut cancel = CancelToken::new();
        cancel.probe = Some(Arc::clone(&state));
        (
            RunContext {
                cancel,
                progress: Progress::new(),
            },
            CheckpointProbe(state),
        )
    }
}

/// `0..n` as a `Vec<u32>`, written with checkpoints: first-touching a few
/// hundred megabytes can take a while when the machine is short of memory.
pub fn indices(n: u32, ctx: &RunContext) -> Result<Vec<u32>, Cancelled> {
    let mut v = Vec::with_capacity(n as usize);
    let mut start = 0u32;
    while start < n {
        ctx.checkpoint()?;
        let end = start.saturating_add(TICK_INTERVAL).min(n);
        v.extend(start..end);
        start = end;
    }
    Ok(v)
}

/// How many [`Ticker::tick`] calls go between checkpoints.
pub const TICK_INTERVAL: u32 = 32_768;

/// Checkpoints every [`TICK_INTERVAL`] ticks (see [`RunContext::ticker`]).
pub struct Ticker<'a> {
    ctx: &'a RunContext,
    n: u32,
}

impl Ticker<'_> {
    /// Counts one unit of work; every [`TICK_INTERVAL`]th call is a
    /// checkpoint.
    #[inline]
    #[track_caller]
    pub fn tick(&mut self) -> Result<(), Cancelled> {
        self.n = self.n.wrapping_add(1);
        if self.n.is_multiple_of(TICK_INTERVAL) {
            self.ctx.checkpoint()
        } else {
            Ok(())
        }
    }
}

/// A share of a stage's progress covering `total` units of work (see
/// [`RunContext::span`]).
pub struct Span<'a> {
    ctx: &'a RunContext,
    stage: &'static str,
    from: f64,
    to: f64,
    total: usize,
}

impl Span<'_> {
    /// Call with the index of each unit of work as it starts (any order,
    /// any thread): every [`TICK_INTERVAL`]th index reports progress and
    /// checkpoints.
    #[inline]
    #[track_caller]
    pub fn at(&self, done: usize) -> Result<(), Cancelled> {
        if done.is_multiple_of(TICK_INTERVAL as usize) {
            self.report(done);
            self.ctx.checkpoint()
        } else {
            Ok(())
        }
    }

    /// Reports `done` of `total` units, and checkpoints. For work whose
    /// units are big enough to check every time.
    #[track_caller]
    pub fn step(&self, done: usize) -> Result<(), Cancelled> {
        self.report(done);
        self.ctx.checkpoint()
    }

    fn report(&self, done: usize) {
        let f = done.min(self.total) as f64 / self.total as f64;
        self.ctx
            .progress
            .report(self.stage, self.from + (self.to - self.from) * f);
    }

    /// Reports the span's end.
    pub fn finish(&self) {
        self.ctx.progress.report(self.stage, self.to);
    }
}

/// The longest stretch between two checkpoints seen by a
/// [`CheckpointProbe`], and where it started and ended.
#[derive(Debug, Clone, Copy)]
pub struct CheckpointGap {
    pub gap: std::time::Duration,
    /// The checkpoint that opened the gap; `None` when it is the probe's
    /// creation.
    pub from: Option<&'static std::panic::Location<'static>>,
    pub to: &'static std::panic::Location<'static>,
}

#[derive(Debug)]
struct ProbeInner {
    last: std::time::Instant,
    last_at: Option<&'static std::panic::Location<'static>>,
    worst: Option<CheckpointGap>,
    count: u64,
}

#[derive(Debug)]
struct ProbeState {
    inner: std::sync::Mutex<ProbeInner>,
}

impl ProbeState {
    fn hit(&self, at: &'static std::panic::Location<'static>) {
        let now = std::time::Instant::now();
        let mut inner = self.inner.lock().expect("checkpoint probe lock poisoned");
        let gap = now.duration_since(inner.last);
        if inner.worst.is_none_or(|w| gap > w.gap) {
            inner.worst = Some(CheckpointGap {
                gap,
                from: inner.last_at,
                to: at,
            });
        }
        inner.last = now;
        inner.last_at = Some(at);
        inner.count += 1;
    }
}

/// Read side of [`RunContext::instrumented`]. Gaps are measured between
/// consecutive checkpoints from any thread, starting at the probe's
/// creation. With several threads, one worker's long silence is hidden by
/// the others' checkpoints, so to measure the worst gap run the work on a
/// one-thread pool ([`JobPool::new`]`(Some(1))`).
#[derive(Debug, Clone)]
pub struct CheckpointProbe(Arc<ProbeState>);

impl CheckpointProbe {
    /// The longest gap so far, if any checkpoint has been made.
    pub fn worst(&self) -> Option<CheckpointGap> {
        self.0
            .inner
            .lock()
            .expect("checkpoint probe lock poisoned")
            .worst
    }

    /// The longest gap so far (zero before the first checkpoint).
    pub fn max_gap(&self) -> std::time::Duration {
        self.worst().map_or(std::time::Duration::ZERO, |w| w.gap)
    }

    /// Checkpoints made so far.
    pub fn checkpoints(&self) -> u64 {
        self.0
            .inner
            .lock()
            .expect("checkpoint probe lock poisoned")
            .count
    }
}

/// How many threads [`JobPool::new`] chose, and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolChoice {
    pub threads: usize,
    pub cores: usize,
    pub reason: &'static str,
}

impl PoolChoice {
    /// How many threads a pool built for `threads` gets (see
    /// [`JobPool::new`]), without building it: for frontends that split
    /// one thread budget over several pools.
    pub fn for_request(threads: Option<usize>) -> PoolChoice {
        // `available_parallelism` only errs when the OS cannot answer at
        // all (e.g. a heavily sandboxed environment); 1 core is then the
        // only safe assumption, not a guess hiding a real failure. Whether
        // that happened is surfaced below in `reason` (not silently
        // conflated with a genuine single-core machine), per MF-RT-004's
        // "shown in diagnostics" requirement.
        // allow-swallow: fallback surfaced in `reason`, see above.
        let detected_cores = std::thread::available_parallelism().map(|n| n.get()).ok();
        let cores = detected_cores.unwrap_or(1);
        let cores_unknown = detected_cores.is_none();

        let (threads, reason): (usize, &'static str) = match (threads, cores_unknown) {
            (None, true) => (
                1,
                "default: all cores but one (core count unavailable; assumed 1 core, using 1 thread)",
            ),
            (None, false) => (cores.saturating_sub(1).max(1), "default: all cores but one"),
            (Some(0), _) => (1, "requested 0 threads; using 1"),
            (Some(n), _) if n > cores => (n, "requested more threads than cores"),
            (Some(n), _) => (n, "requested thread count honoured"),
        };
        PoolChoice {
            threads,
            cores,
            reason,
        }
    }
}

/// A named `rayon` thread pool for background work.
///
/// Threads are named `mf-pool-0`, `mf-pool-1`, ... so they are
/// identifiable in a debugger or profiler.
pub struct JobPool {
    pool: rayon::ThreadPool,
    /// Tasks spawned and not yet returned (see [`JobPool::in_flight`]).
    in_flight: Arc<AtomicUsize>,
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
        Self::named("mf-pool", threads)
    }

    /// As [`JobPool::new`], with threads named `{name}-0`, `{name}-1`, ...
    /// (for example a separate small pool for latency-sensitive work).
    pub fn named(name: &'static str, threads: Option<usize>) -> (Self, PoolChoice) {
        let choice = PoolChoice::for_request(threads);
        let chosen = choice.threads;
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(chosen)
            .thread_name(move |i| format!("{name}-{i}"))
            .build()
            .expect("failed to build mf-runtime job pool");

        (
            Self {
                pool,
                in_flight: Arc::default(),
            },
            choice,
        )
    }

    /// Runs `f` on one of this pool's threads without waiting for it.
    /// Parallel work inside `f` (rayon iterators, joins) runs on this pool.
    ///
    /// A panic in `f` aborts the process (rayon's default), so callers
    /// that must survive one catch it inside `f`.
    pub fn spawn(&self, f: impl FnOnce() + Send + 'static) {
        let in_flight = Arc::clone(&self.in_flight);
        in_flight.fetch_add(1, Ordering::AcqRel);
        self.pool.spawn(move || {
            // Counted down however `f` ends: a panic aborts anyway, but
            // the count must never stay up for a task that is gone.
            struct Done(Arc<AtomicUsize>);
            impl Drop for Done {
                fn drop(&mut self) {
                    self.0.fetch_sub(1, Ordering::AcqRel);
                }
            }
            let _done = Done(in_flight);
            f();
        })
    }

    /// Tasks [`JobPool::spawn`]ed and not yet returned, queued or running.
    /// Whatever a task did before returning (sent a message, asked for a
    /// repaint) has happened once it no longer counts. For tests and
    /// diagnostics: "is everything handed to the pool done?"
    pub fn in_flight(&self) -> usize {
        self.in_flight.load(Ordering::Acquire)
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
/// integer) into one `u64` so [`Progress`] can update "which stage" and
/// "how far along" as a single atomic unit: no locks, no allocation,
/// lock-free for readers, and no window where a reader or a stale writer
/// can observe one half of the pair updated and not the other.
const FRACTION_SCALE: u64 = 1_000_000;

fn pack(stage_index: u32, fraction: f64) -> u64 {
    let scaled = (fraction.clamp(0.0, 1.0) * FRACTION_SCALE as f64).round() as u64;
    (u64::from(stage_index) << 32) | scaled.min(FRACTION_SCALE)
}

fn unpack(bits: u64) -> (u32, f64) {
    let stage_index = (bits >> 32) as u32;
    let scaled = bits & 0xFFFF_FFFF;
    (stage_index, scaled as f64 / FRACTION_SCALE as f64)
}

/// A `(stage, fraction)` update, as sent over a [`Progress::channel`].
type ProgressUpdate = (&'static str, f64);

/// The optional fan-out sink a [`Progress`] sends updates to.
type ProgressSink = Arc<std::sync::Mutex<Option<mpsc::Sender<ProgressUpdate>>>>;

/// An append-only table mapping stage names to stable indices, so a stage's
/// *identity* can live in the same atomic as its fraction (see [`pack`]).
/// Lookup/insert takes a brief lock only when a never-seen-before stage
/// name is reported; reading an already-known index back out (via
/// [`StageTable::name`]) never blocks a concurrent insert because entries
/// are only ever appended, never mutated or removed, so a snapshot of the
/// `Vec` taken under a read lock stays valid after the lock is released.
#[derive(Debug, Default)]
struct StageTable {
    names: std::sync::RwLock<Vec<&'static str>>,
}

impl StageTable {
    fn index_of(&self, stage: &'static str) -> u32 {
        {
            let names = self.names.read().expect("stage table lock poisoned");
            if let Some(index) = names.iter().position(|&s| s == stage) {
                return index as u32;
            }
        }
        // Not seen before: upgrade to a write lock. Re-check first, since
        // another thread may have inserted the same stage name while we
        // only held (and released) the read lock above.
        let mut names = self.names.write().expect("stage table lock poisoned");
        if let Some(index) = names.iter().position(|&s| s == stage) {
            return index as u32;
        }
        names.push(stage);
        (names.len() - 1) as u32
    }

    fn name(&self, index: u32) -> &'static str {
        let names = self.names.read().expect("stage table lock poisoned");
        names
            .get(index as usize)
            .copied()
            .unwrap_or("<unknown stage>")
    }
}

/// Lock-free, monotonic progress reporting for one background job.
///
/// Within a single stage (the same `stage` string reported consecutively),
/// the fraction never decreases, even if a caller reports a smaller value
/// (e.g. a stale update racing a newer one). Starting a new stage resets
/// the monotonic floor. The fraction is always clamped to `0.0..=1.0`.
///
/// `snapshot` is a single atomic load of the packed `(stage index,
/// fraction)` word, and `report` a single CAS loop on it, so a reader
/// (e.g. a UI polling every frame) never blocks and can never observe a
/// stage paired with another stage's fraction. Writers serialise on the
/// channel sink's lock (held across the CAS) so a [`Progress::channel`]
/// reader receives updates in exactly the order they were committed.
#[derive(Debug, Clone)]
pub struct Progress {
    // Bit-packed (stage index, fraction), updated as a single atomic unit.
    state: Arc<AtomicU64>,
    // Stage names behind their stable index (see `pack`/`unpack`).
    stages: Arc<StageTable>,
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
        let stages = StageTable::default();
        let empty_index = stages.index_of("");
        Self {
            state: Arc::new(AtomicU64::new(pack(empty_index, 0.0))),
            stages: Arc::new(stages),
            sink: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// Reports progress within `stage`. `fraction` is clamped to
    /// `0.0..=1.0` and, within the same stage, never allowed to move
    /// backwards from what was last reported.
    ///
    /// Stage identity and fraction are committed together in a single CAS
    /// loop on the packed atomic, so a writer only ever applies a
    /// same-stage "never go backwards" update if the stage it read is
    /// still the current one by the time it commits; if a concurrent
    /// transition to a different stage won the race first, this writer's
    /// CAS fails, it re-reads the now-current (different) stage/fraction,
    /// and retries — it can never clobber a fresh stage's reset fraction
    /// with a stale value computed against the old stage.
    pub fn report(&self, stage: &'static str, fraction: f64) {
        let clamped = fraction.clamp(0.0, 1.0);
        let stage_index = self.stages.index_of(stage);

        // The sink lock is held across the update so a channel reader sees
        // updates in the order they were applied: two concurrent writers
        // can't send in the opposite order to their commits (and a reader
        // would see progress regress). `snapshot` never takes this lock.
        let sink = self.sink.lock().expect("progress sink lock poisoned");
        let mut committed = clamped;
        // `try_update` returns the *previous* value, so the committed
        // fraction is captured from the closure's last (successful) run.
        self.state
            .try_update(Ordering::Release, Ordering::Acquire, |bits| {
                let (current_index, current_fraction) = unpack(bits);
                let next_fraction = if current_index == stage_index {
                    // Same stage as the atomic currently holds: never move
                    // the fraction backwards.
                    clamped.max(current_fraction)
                } else {
                    // A different stage than what's currently live (either
                    // a genuine transition, or this stage hasn't won the
                    // race to become current yet): reset the floor.
                    clamped
                };
                let next = pack(stage_index, next_fraction);
                committed = unpack(next).1;
                Some(next)
            })
            .expect("try_update closure always returns Some");

        if let Some(sender) = sink.as_ref() {
            // allow-swallow: best-effort fan-out; a dropped reader must not fail reporting.
            let _ = sender.send((stage, committed));
        }
    }

    /// Reads the current `(stage, fraction)`. Lock-free on the fast path:
    /// a single atomic load for the packed word, then a stage-name lookup
    /// that never blocks a concurrent `report` (stage names are only ever
    /// appended, never rewritten, so the lookup's read lock and a writer's
    /// insert cannot race on the same entry).
    pub fn snapshot(&self) -> (&'static str, f64) {
        let (stage_index, fraction) = unpack(self.state.load(Ordering::Acquire));
        (self.stages.name(stage_index), fraction)
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
