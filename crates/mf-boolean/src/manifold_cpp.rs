//! [`ManifoldCpp`]: the [`Kernel`] adapter for the C++ Manifold library
//! (elalish/manifold v3.5.3, Apache-2.0) through `mf-manifold-sys`, Phase 1
//! Task 7b (ADR-0002). Behind the `manifold-cpp` feature.
//!
//! - **Engine.** Manifold's float boolean: symbolic perturbation keeps every
//!   decision consistent, but constructed points are rounded f64. Its
//!   import refuses an open or inconsistently wound operand as
//!   `NotManifold`, which the adapter names [`KernelError::NotClosed`] when
//!   the operand has an open edge (an edge on four faces it accepts, by
//!   splitting the edge's vertices). It assumes operands that don't
//!   intersect themselves: on scans that do, Task 7b measured invalid
//!   output (overlapping sheets, vertices off their faces). So the adapter
//!   first checks each operand exactly (`self_intersecting_faces`) and
//!   refuses one that intersects itself as
//!   [`KernelError::SelfIntersecting`]. Under D5 the operand is the
//!   window, so only self-intersections near the tool refuse.
//! - **Provenance.** Each input triangle is its own Manifold `faceID`, so
//!   Manifold never merges two input triangles into one output triangle and
//!   every output triangle names exactly one input triangle; the adapter
//!   maps it to the caller's label.
//! - **Cancel.** Import and boolean both observe Manifold's
//!   `ExecutionContext`; a watcher thread forwards our token to it every
//!   2 ms. The C++ call returns at its next check, so the computation never
//!   outlives the call (nothing is detached).
//! - **Threads.** The boolean runs in a oneTBB arena as wide as the current
//!   `rayon` pool, so `JobPool::install` controls it as for the Rust kernel.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use mf_manifold_sys as sys;
use mf_runtime::RunContext;

use crate::kernel::{BoolOp, Kernel, KernelError, KernelMesh, KernelResult};
use crate::validate::{SolidError, has_open_edge, self_intersecting_faces};

/// The C++ Manifold kernel behind [`Kernel`]. `ManifoldCpp::default()`
/// refuses operands that intersect themselves (see the module docs).
#[derive(Debug, Clone, Copy)]
pub struct ManifoldCpp {
    check_operands: bool,
}

impl Default for ManifoldCpp {
    fn default() -> Self {
        ManifoldCpp {
            check_operands: true,
        }
    }
}

impl ManifoldCpp {
    /// With the operand self-intersection check (the default; the shipping
    /// kernel).
    pub const fn checked() -> Self {
        ManifoldCpp {
            check_operands: true,
        }
    }

    /// Without the self-intersection check: Manifold computes whatever it
    /// computes, and only D4's validation stands between it and the user.
    /// For the Task 7b qualification (to measure what the check prevents,
    /// and to cut windows from whole scans), not for the product.
    pub fn unchecked() -> Self {
        ManifoldCpp {
            check_operands: false,
        }
    }
}

const POLL: Duration = Duration::from_millis(2);

fn check_operand(m: &KernelMesh, operand: u8) -> Result<(), KernelError> {
    if m.face_ids.len() != m.triangles.len() {
        return Err(KernelError::Internal(format!(
            "operand {operand}: {} face labels for {} triangles",
            m.face_ids.len(),
            m.triangles.len()
        )));
    }
    let n = m.positions.len() as u64;
    if let Some(f) = m
        .triangles
        .iter()
        .position(|t| t.iter().any(|&v| u64::from(v) >= n))
    {
        return Err(KernelError::Internal(format!(
            "operand {operand}: triangle {f} names a vertex that doesn't exist"
        )));
    }
    if let Some(v) = m
        .positions
        .iter()
        .position(|p| p.iter().any(|c| !c.is_finite()))
    {
        return Err(KernelError::Internal(format!(
            "operand {operand}: vertex {v} has a non-finite coordinate"
        )));
    }
    Ok(())
}

fn wide(m: &KernelMesh) -> Vec<[u64; 3]> {
    m.triangles.iter().map(|t| t.map(u64::from)).collect()
}

/// The cancel watcher (see the module docs). Dropping it stops and joins
/// the thread, so a panic anywhere between `start` and `finish` (unwinding
/// through this frame) can't leave it running with the Manifold context
/// (Task 7b review I1).
pub(crate) struct Watcher {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Watcher {
    pub(crate) fn start(theirs: &Arc<sys::Context>, ctx: &RunContext) -> Watcher {
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (theirs, stop) = (Arc::clone(theirs), Arc::clone(&stop));
            let ours = ctx.cancel.clone();
            let progress = ctx.progress.clone();
            std::thread::Builder::new()
                .name("mf-boolean-cancel".into())
                .spawn(move || {
                    let mut best: f64 = 0.0;
                    while !stop.load(Ordering::Relaxed) {
                        if ours.is_cancelled() {
                            theirs.cancel();
                            return;
                        }
                        let p = theirs.progress();
                        // Manifold reports 1.0 before any work is scheduled.
                        if p < 1.0 && p > best {
                            best = p;
                            progress.report("boolean: manifold", 0.05 + 0.9 * p);
                        }
                        std::thread::park_timeout(POLL);
                    }
                })
                .expect("could not start the boolean cancel watcher thread")
        };
        Watcher {
            stop,
            thread: Some(thread),
        }
    }

    /// Whether the thread is still running (for tests).
    #[cfg(test)]
    fn handle(&self) -> Option<&std::thread::JoinHandle<()>> {
        self.thread.as_ref()
    }

    /// Stops and joins the thread; a panic on it is re-raised here.
    pub(crate) fn finish(mut self) {
        if let Some(panic) = self.stop_and_join() {
            std::panic::resume_unwind(panic);
        }
    }

    fn stop_and_join(&mut self) -> Option<Box<dyn std::any::Any + Send>> {
        let thread = self.thread.take()?;
        self.stop.store(true, Ordering::Relaxed);
        thread.thread().unpark();
        thread.join().err()
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        // Unwinding already, or finished: never panic again here.
        let _ = self.stop_and_join(); // allow-swallow: a watcher panic during unwind can't be re-raised
    }
}

impl Kernel for ManifoldCpp {
    fn boolean(
        &self,
        a: &KernelMesh,
        b: &KernelMesh,
        op: BoolOp,
        ctx: &RunContext,
    ) -> Result<KernelResult, KernelError> {
        ctx.checkpoint().map_err(|_| KernelError::Cancelled)?;
        check_operand(a, 0)?;
        check_operand(b, 1)?;
        // Manifold's contract: operands that don't intersect themselves
        // (Task 7b measured invalid output on scans that do). Checked
        // exactly, and refused by name rather than computed.
        for (operand, m) in [(0u8, a), (1, b)]
            .into_iter()
            .filter(|_| self.check_operands)
        {
            // Open first, as manifold-rust names it: the more basic defect.
            if has_open_edge(&m.triangles) {
                return Err(KernelError::NotClosed { operand });
            }
            let hits =
                self_intersecting_faces(&m.positions, &m.triangles, ctx).map_err(|e| match e {
                    SolidError::Cancelled => KernelError::Cancelled,
                    e => KernelError::Internal(format!("operand {operand}: {e}")),
                })?;
            let faces = hits.iter().filter(|&&h| h).count();
            if faces > 0 {
                return Err(KernelError::SelfIntersecting { operand, faces });
            }
        }
        let (ta, tb) = (wide(a), wide(b));
        let theirs = Arc::new(sys::Context::new());
        let watcher = Watcher::start(&theirs, ctx);
        ctx.progress.report("boolean: manifold", 0.0);
        let sop = match op {
            BoolOp::Union => sys::Op::Union,
            BoolOp::Subtract => sys::Op::Subtract,
            BoolOp::Intersect => sys::Op::Intersect,
        };
        let result = sys::boolean(
            &sys::MeshIn {
                positions: &a.positions,
                triangles: &ta,
            },
            &sys::MeshIn {
                positions: &b.positions,
                triangles: &tb,
            },
            sop,
            Some(rayon::current_num_threads()),
            &theirs,
        );
        watcher.finish();
        // A run the user cancelled reports Cancelled even if it finished.
        ctx.checkpoint().map_err(|_| KernelError::Cancelled)?;
        let out = match result {
            Ok(out) => out,
            Err(sys::Error::Cancelled) => return Err(KernelError::Cancelled),
            Err(sys::Error::Import {
                operand,
                status: "NotManifold",
            }) => {
                let m = if operand == 0 { a } else { b };
                return Err(if has_open_edge(&m.triangles) {
                    KernelError::NotClosed { operand }
                } else {
                    KernelError::Internal(format!(
                        "operand {operand}: not an oriented surface (faces wound \
                         inconsistently); Manifold: NotManifold"
                    ))
                });
            }
            Err(sys::Error::Import { operand, status }) => {
                return Err(KernelError::Internal(format!(
                    "operand {operand}: Manifold: {status}"
                )));
            }
            Err(e) => return Err(KernelError::Internal(format!("Manifold: {e:?}"))),
        };
        let mut triangles = Vec::with_capacity(out.triangles.len());
        let n = out.positions.len() as u64;
        for t in &out.triangles {
            if t.iter().any(|&v| v >= n) {
                return Err(KernelError::Internal(
                    "kernel output names a vertex that doesn't exist".into(),
                ));
            }
            triangles.push(t.map(|v| v as u32));
        }
        let operands = [a, b];
        let origin = out
            .operand
            .iter()
            .zip(&out.face)
            .enumerate()
            .map(|(t, (&op, &face))| {
                operands
                    .get(op as usize)
                    .and_then(|m| m.face_ids.get(face as usize))
                    .copied()
                    .ok_or_else(|| {
                        KernelError::Internal(format!(
                            "kernel output triangle {t} names face {face} of operand {op}, which doesn't exist"
                        ))
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        ctx.progress.report("boolean: manifold", 1.0);
        Ok(KernelResult {
            positions: out.positions,
            triangles,
            origin,
        })
    }

    fn id(&self) -> &'static str {
        if self.check_operands {
            "manifold3d=3.5.3/c++"
        } else {
            "manifold3d=3.5.3/c++ (unchecked operands)"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panic_while_the_watcher_runs_still_stops_and_joins_it() {
        let theirs = Arc::new(sys::Context::new());
        let ctx = RunContext::none();
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let watcher = Watcher::start(&theirs, &ctx);
            assert!(watcher.handle().is_some());
            assert_eq!(Arc::strong_count(&theirs), 2);
            panic!("a fallible step inside the kernel call");
        }));
        assert!(caught.is_err());
        // The guard joined the thread while unwinding: only this test's
        // reference to the Manifold context is left.
        assert_eq!(Arc::strong_count(&theirs), 1);
    }
}
