//! The one job system's preview and background lanes (ADR-0009,
//! architecture-review.md §3). Next to the serial command queue
//! (`MeshFixerApp::run_invocation`) and the pick/hover lane
//! (`crate::worker::Workers::pick`), everything a tool computes in the
//! background goes through here:
//!
//! - [`PreviewSlot`]: latest request wins, one at a time, on the shared
//!   background pool. Every request gets a [`Ticket`]; a result is
//!   current only while its ticket is the slot's latest and the slot
//!   hasn't been cancelled since ([`PreviewSlot::is_current`]), so a
//!   result for an older key or base revision is never accepted. A
//!   tool-owned request ([`PreviewSlot::submit_for`]) is cancelled when
//!   its owner tool closes ([`PreviewSlot::release_unless_owner`]).
//! - [`Jobs::once`]: a single cancellable task (a hinge swing check, a
//!   thumbnail, a joint's geometry), superseding the one before it when
//!   given the same token cell.
//!
//! Both are panic-safe: a panic is caught on the worker and reported
//! (a [`Message::Failed`] for the app's slots, a [`Failure::Panicked`]
//! to a one-shot task's own `done`), never left to abort the app nor to
//! look like work forever. Both run on the background pool, so
//! `MeshFixerApp::background_work_in_flight` (what `settle()` waits on)
//! counts them.
//!
//! The only places in mf-desktop that may start a thread or make an
//! uncancellable [`RunContext`] are this module, `worker.rs` and
//! `main.rs` (`tests/lint_jobs.rs`).

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use eframe::egui;
use mf_runtime::{CancelToken, Cancelled, JobPool, RunContext};

use crate::worker::Message;

/// A request's number in its slot: a result is current only while its
/// ticket is the slot's latest ([`PreviewSlot::is_current`]).
pub type Ticket = u64;

/// Who a tool-owned preview belongs to: the tool's id (`active_tool`).
pub type Owner = &'static str;

/// A fresh context, cancellable through its token: the lane module's own,
/// for a job's [`RunContext`].
pub fn context() -> RunContext {
    RunContext::none()
}

/// A panic payload in words.
pub fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    match payload.downcast::<String>() {
        Ok(s) => *s,
        Err(payload) => match payload.downcast::<&'static str>() {
            Ok(s) => (*s).to_owned(),
            Err(_) => "unknown panic".to_owned(),
        },
    }
}

/// Why a one-shot task ([`Jobs::once`]) has no result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// Superseded or cancelled: nobody is waiting for it.
    Cancelled,
    /// The work panicked, with this message.
    Panicked(String),
}

impl From<Cancelled> for Failure {
    fn from(_: Cancelled) -> Self {
        Failure::Cancelled
    }
}

struct State<T> {
    pending: Option<(Ticket, T, RunContext)>,
    /// A drain loop is on the pool.
    running: bool,
    /// The newest request's token, cancelled when it is superseded.
    latest: Option<CancelToken>,
    /// The newest request's ticket; bumped by [`PreviewSlot::cancel`] too,
    /// so nothing outstanding is current after a cancel.
    generation: Ticket,
    /// The tool the newest request belongs to, if a tool's.
    owner: Option<Owner>,
    shutdown: bool,
}

/// Runs one request with its ticket and context.
type Run<T> = Arc<dyn Fn(Ticket, T, &RunContext) + Send + Sync>;
/// Told about a request whose work panicked.
type Failed = Arc<dyn Fn(Ticket, String) + Send + Sync>;

/// Runs only the most recent request, on a shared pool (see the module
/// docs).
pub struct PreviewSlot<T: Send + 'static> {
    state: Arc<Mutex<State<T>>>,
    pool: Arc<JobPool>,
    run: Run<T>,
    failed: Failed,
}

impl<T: Send + 'static> PreviewSlot<T> {
    /// `work` returns `Err(Cancelled)` once a newer request (or a cancel)
    /// has superseded it; only finished work reaches `done`, with its
    /// request's ticket. A panic in `work` or `done` reaches `failed`
    /// instead, and the slot goes on to the next request.
    pub fn new<R: 'static>(
        pool: Arc<JobPool>,
        work: impl Fn(T, &RunContext) -> Result<R, Cancelled> + Send + Sync + 'static,
        done: impl Fn(Ticket, R) + Send + Sync + 'static,
        failed: impl Fn(Ticket, String) + Send + Sync + 'static,
    ) -> PreviewSlot<T> {
        PreviewSlot {
            state: Arc::new(Mutex::new(State {
                pending: None,
                running: false,
                latest: None,
                generation: 0,
                owner: None,
                shutdown: false,
            })),
            pool,
            run: Arc::new(move |ticket, request, ctx| {
                // Err is a superseded request: there's nothing to report.
                if let Ok(result) = work(request, ctx) {
                    done(ticket, result);
                }
            }),
            failed: Arc::new(failed),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State<T>> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Queue `request` (the app's own, owned by no tool), cancelling the
    /// one running and replacing any that hasn't started yet.
    pub fn submit(&self, request: T) -> Ticket {
        self.submit_owned(None, request)
    }

    /// As [`PreviewSlot::submit`], for a preview tool `owner` shows: it is
    /// cancelled once that tool closes ([`Self::release_unless_owner`]).
    pub fn submit_for(&self, owner: Owner, request: T) -> Ticket {
        self.submit_owned(Some(owner), request)
    }

    fn submit_owned(&self, owner: Option<Owner>, request: T) -> Ticket {
        let mut state = self.lock();
        if let Some(old) = state.latest.take() {
            old.cancel();
        }
        let ctx = context();
        state.generation += 1;
        let ticket = state.generation;
        state.latest = Some(ctx.cancel.clone());
        state.pending = Some((ticket, request, ctx));
        state.owner = owner;
        if state.running {
            return ticket;
        }
        state.running = true;
        let (shared, run, failed) = (self.state.clone(), self.run.clone(), self.failed.clone());
        self.pool.spawn(move || {
            loop {
                let (ticket, request, ctx) = {
                    let mut state = shared.lock().unwrap_or_else(|p| p.into_inner());
                    match state.pending.take() {
                        Some(next) if !state.shutdown => next,
                        _ => {
                            state.running = false;
                            return;
                        }
                    }
                };
                // A panic is this request's failure, reported; the slot
                // (and the pool thread under it) carries on.
                let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run(ticket, request, &ctx)
                }));
                if let Err(payload) = ran {
                    let message = panic_message(payload);
                    // allow-swallow: a panicking failure report has nobody left to tell; the next request still runs
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        failed(ticket, message)
                    }));
                }
            }
        });
        ticket
    }

    /// Whether `ticket`'s result is still wanted: it is the latest request
    /// and nothing has cancelled it since.
    pub fn is_current(&self, ticket: Ticket) -> bool {
        self.lock().generation == ticket
    }

    /// Cancel the running request and drop any not yet started: nothing
    /// outstanding is current afterwards.
    pub fn cancel(&self) {
        let mut state = self.lock();
        if let Some(t) = state.latest.take() {
            t.cancel();
        }
        state.pending = None;
        state.generation += 1;
        state.owner = None;
    }

    /// Cancel the latest request if a tool other than `open` owns it (its
    /// owner closed). Returns whether it did.
    pub fn release_unless_owner(&self, open: Owner) -> bool {
        let owned_elsewhere = self.lock().owner.is_some_and(|o| o != open);
        if owned_elsewhere {
            self.cancel();
        }
        owned_elsewhere
    }

    /// The tool the latest request belongs to (`None` once cancelled, or
    /// for the app's own requests).
    pub fn owner(&self) -> Option<Owner> {
        self.lock().owner
    }

    /// Whether a request is queued or running.
    pub fn busy(&self) -> bool {
        let state = self.lock();
        state.running || state.pending.is_some()
    }
}

impl<T: Send + 'static> Drop for PreviewSlot<T> {
    fn drop(&mut self) {
        let mut state = self.lock();
        state.shutdown = true;
        state.pending = None;
        // Don't block shutdown on a long preview: cancel it.
        if let Some(t) = state.latest.take() {
            t.cancel();
        }
    }
}

/// The app's handle on the background lane: makes [`PreviewSlot`]s whose
/// panics are reported as [`Message::Failed`], and runs one-shot tasks.
#[derive(Clone)]
pub struct Jobs {
    pool: Arc<JobPool>,
    tx: Sender<Message>,
    repaint: egui::Context,
}

impl Jobs {
    pub fn new(pool: Arc<JobPool>, tx: Sender<Message>, repaint: egui::Context) -> Jobs {
        Jobs { pool, tx, repaint }
    }

    /// The pool the lane runs on.
    pub fn pool(&self) -> Arc<JobPool> {
        self.pool.clone()
    }

    /// Send `msg` to the app and ask for a frame to handle it in.
    pub fn send(&self, msg: Message) {
        // allow-swallow: the receiver only disappears while the app is closing
        let _ = self.tx.send(msg);
        self.repaint.request_repaint();
    }

    /// Ask for a frame (a polled result landed).
    pub fn repaint(&self) {
        self.repaint.request_repaint();
    }

    /// A [`PreviewSlot`] on the background pool whose panics the app
    /// reports as a [`Message::Failed`] for `what`.
    pub fn preview<T: Send + 'static, R: 'static>(
        &self,
        what: &'static str,
        work: impl Fn(T, &RunContext) -> Result<R, Cancelled> + Send + Sync + 'static,
        done: impl Fn(Ticket, R) + Send + Sync + 'static,
    ) -> PreviewSlot<T> {
        let jobs = self.clone();
        PreviewSlot::new(self.pool.clone(), work, done, move |_, message| {
            jobs.send(Message::Failed {
                what,
                job: false,
                message,
            });
        })
    }

    /// Run `work` once on the background pool, then `done` with what it
    /// returned, `Failure::Cancelled` if it was cancelled, or
    /// `Failure::Panicked` if it panicked. `latest` holds the token of the
    /// task this one supersedes (cancelled now) and then this one's, so
    /// whoever holds it can cancel it (its owner closing).
    pub fn once<R: Send + 'static>(
        &self,
        latest: &mut Option<CancelToken>,
        work: impl FnOnce(&RunContext) -> Result<R, Cancelled> + Send + 'static,
        done: impl FnOnce(Result<R, Failure>) + Send + 'static,
    ) {
        if let Some(old) = latest.take() {
            old.cancel();
        }
        let ctx = context();
        *latest = Some(ctx.cancel.clone());
        let repaint = self.repaint.clone();
        self.pool.spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&ctx)))
                .map_err(|payload| Failure::Panicked(panic_message(payload)))
                .and_then(|r| r.map_err(Failure::from));
            done(result);
            repaint.request_repaint();
        });
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // tests may panic on failure
mod tests {
    use super::*;
    use std::sync::mpsc::channel;
    use std::time::Duration;

    fn pool() -> Arc<JobPool> {
        Arc::new(JobPool::new(Some(2)).0)
    }

    /// A panicking request is reported with its ticket, and the slot runs
    /// the next one: the pool thread under it isn't lost and nothing
    /// aborts.
    #[test]
    fn a_panicking_request_is_reported_and_the_slot_carries_on() {
        let (tx, rx) = channel();
        let (ftx, frx) = channel();
        let slot = PreviewSlot::new(
            pool(),
            |n: u32, _: &RunContext| {
                assert!(n != 0, "injected preview failure");
                Ok(n)
            },
            move |t, n| tx.send((t, n)).unwrap(),
            move |t, m| ftx.send((t, m)).unwrap(),
        );
        let first = slot.submit(0);
        let (t, m) = frx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(t, first);
        assert!(m.contains("injected preview failure"), "{m}");
        let second = slot.submit(7);
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            (second, 7)
        );
    }

    /// A cancel stops the running request (its context says so), drops
    /// any queued one, and makes every outstanding ticket stale.
    #[test]
    fn cancel_stops_the_running_request_and_staleness_follows() {
        let (started_tx, started_rx) = channel();
        let (tx, rx) = channel::<Ticket>();
        let slot = PreviewSlot::new(
            pool(),
            move |_: (), ctx: &RunContext| {
                started_tx.send(()).unwrap();
                let start = std::time::Instant::now();
                while start.elapsed() < Duration::from_secs(10) {
                    ctx.checkpoint()?;
                    std::thread::sleep(Duration::from_millis(1));
                }
                Ok(())
            },
            move |t, ()| tx.send(t).unwrap(),
            |_, m| panic!("{m}"),
        );
        let t = slot.submit_for("tool", ());
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(slot.is_current(t));
        assert!(!slot.release_unless_owner("tool"), "its owner is open");
        assert!(slot.release_unless_owner("other"), "its owner closed");
        assert!(!slot.is_current(t));
        assert_eq!(slot.owner(), None);
        let start = std::time::Instant::now();
        while slot.busy() {
            assert!(start.elapsed() < Duration::from_secs(2), "never stopped");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            rx.try_recv().is_err(),
            "a cancelled request reports nothing"
        );
    }
}
