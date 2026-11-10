//! [`ManifoldRobust`]: the [`Kernel`] adapter for `manifold-rust` 0.19.2's
//! robust engine (D1): exact rational arithmetic on a mesh arrangement,
//! closed-orientable soup in, a paired manifold out.
//!
//! What the adapter does, checked against the 0.19.2 source (ADR-0002
//! lists where it differs from the plan's reading of the docs):
//! - Operands go in as `MeshGL64` through `Manifold::from_mesh_gl64_robust`,
//!   which accepts non-manifold soup and refuses an open or non-orientable
//!   surface with `Error::NotClosed`. Each operand gets its own reserved
//!   original ID, and each triangle its index as `face_id`.
//! - The boolean is `Manifold::boolean_with_engine_rule_and_progress` with
//!   `BooleanEngine::Robust` and `WindingRule::Positive` (`{w ≥ 1}`): the
//!   public entry point that dispatches straight to
//!   `robust::boolean_with_rule` (the plan named `robust::boolean_with_progress`,
//!   which takes `ManifoldImpl`s, not `MeshGL64`; it is the same pipeline).
//!   The fast float engine is never called.
//! - Provenance: the robust engine gives every output triangle the
//!   `TriRef` of the input triangle it is a piece of, and `get_mesh_gl64`
//!   exports it as the run's original ID and the triangle's `face_id`. The
//!   adapter maps those back to the caller's `face_ids` labels.
//! - Cancel and progress go through [`bridge`].

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use manifold_rust::cancel::CancelToken as MfCancel;
use manifold_rust::manifold::Manifold;
use manifold_rust::progress::{Phase, ProgressReporter};
use manifold_rust::types::{BooleanEngine, Error as MfError, MeshGL64, OpType, WindingRule};
use mf_runtime::{Progress, RunContext};

use crate::kernel::{BoolOp, Kernel, KernelError, KernelMesh, KernelResult};

/// `manifold-rust`'s robust (exact) engine behind [`Kernel`].
#[derive(Debug, Clone, Copy, Default)]
pub struct ManifoldRobust;

/// How often the bridge's watcher polls our cancel token.
const POLL: Duration = Duration::from_millis(2);

/// Progress bands: importing the operands, the engine's pipeline, export.
const IMPORT_END: f64 = 0.05;
const ENGINE_END: f64 = 0.95;
/// The robust engine's phases are the first nine of `Phase::PIPELINE_ORDER`.
const ENGINE_PHASES: f64 = 9.0;

/// Keeps the bridge running: a watcher thread forwarding our cancel to the
/// kernel's token, and a progress reporter writing the kernel's phases onto
/// our [`Progress`]. Dropping it stops the watcher (promptly: it is
/// unparked) and joins it.
pub struct BridgeGuard {
    stop: Arc<AtomicBool>,
    watcher: Option<JoinHandle<()>>,
    reporter: ProgressReporter,
    progress: MonotonicProgress,
}

impl BridgeGuard {
    /// The reporter to hand the kernel.
    pub fn reporter(&self) -> &ProgressReporter {
        &self.reporter
    }

    /// Reports `fraction` of the whole boolean under `stage`.
    fn report(&self, stage: &'static str, fraction: f64) {
        self.progress.report(stage, fraction);
    }
}

impl Drop for BridgeGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(watcher) = self.watcher.take() {
            watcher.thread().unpark();
            // A panic in the watcher (it has no code that can panic) would
            // be re-raised here rather than hidden.
            if let Err(panic) = watcher.join() {
                std::panic::resume_unwind(panic);
            }
        }
    }
}

/// Our progress, never moving backwards across the kernel's phases (the
/// kernel's fraction restarts in each phase; ours is the whole boolean).
#[derive(Clone)]
struct MonotonicProgress {
    progress: Progress,
    best: Arc<AtomicU64>,
}

impl MonotonicProgress {
    fn report(&self, stage: &'static str, fraction: f64) {
        let bits = fraction.clamp(0.0, 1.0).to_bits();
        // Positive f64 bit patterns order like the values.
        let prev = self.best.fetch_max(bits, Ordering::Relaxed);
        self.progress.report(stage, f64::from_bits(prev.max(bits)));
    }
}

fn stage(phase: Phase) -> &'static str {
    match phase {
        Phase::NarrowPhase => "boolean: narrow phase",
        Phase::SelfIntersections => "boolean: self intersections",
        Phase::CoplanarOverlaps => "boolean: coplanar overlaps",
        Phase::CandidatePoints => "boolean: candidate points",
        Phase::Registries => "boolean: registries",
        Phase::Arrangements => "boolean: arrangements",
        Phase::Cells => "boolean: cells",
        Phase::Winding => "boolean: winding",
        Phase::Assemble => "boolean: assemble",
        Phase::ExactBoolean => "boolean: exact boolean",
        Phase::Minkowski => "boolean: minkowski",
    }
}

/// Connects a run's [`RunContext`] to the kernel: the returned token is
/// cancelled within 10 ms of `ctx.cancel` (a watcher polls every 2 ms),
/// and the guard's [`BridgeGuard::reporter`] maps the kernel's phases onto
/// `ctx.progress` (stages `"boolean: <phase>"`, fractions monotonic over
/// the whole boolean).
///
/// The watcher's polls go through `ctx.cancel`, so for a context made by
/// `RunContext::instrumented` they count as checkpoints: measure the
/// kernel's cancel latency by wall time instead (`qualify_cancel` does).
pub fn bridge(ctx: &RunContext) -> (MfCancel, BridgeGuard) {
    let token = MfCancel::new();
    let stop = Arc::new(AtomicBool::new(false));
    let ours = ctx.cancel.clone();
    let theirs = token.clone();
    let stopped = Arc::clone(&stop);
    let watcher = std::thread::Builder::new()
        .name("mf-boolean-cancel".into())
        .spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                if ours.is_cancelled() {
                    theirs.cancel();
                    return;
                }
                std::thread::park_timeout(POLL);
            }
        })
        .expect("could not start the boolean cancel watcher thread");

    let progress = MonotonicProgress {
        progress: ctx.progress.clone(),
        best: Arc::new(AtomicU64::new(0)),
    };
    let sink = progress.clone();
    let reporter = ProgressReporter::new(move |phase: Phase, fraction: Option<f64>| {
        let position = phase.pipeline_position() as f64;
        let within = fraction.unwrap_or(0.0).clamp(0.0, 1.0);
        let engine = ((position + within) / ENGINE_PHASES).min(1.0);
        sink.report(
            stage(phase),
            IMPORT_END + (ENGINE_END - IMPORT_END) * engine,
        );
    });
    (
        token,
        BridgeGuard {
            stop,
            watcher: Some(watcher),
            reporter,
            progress,
        },
    )
}

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

fn to_mesh_gl(m: &KernelMesh, original_id: u32) -> MeshGL64 {
    MeshGL64 {
        num_prop: 3,
        vert_properties: m.positions.iter().flatten().copied().collect(),
        tri_verts: m
            .triangles
            .iter()
            .flatten()
            .map(|&v| u64::from(v))
            .collect(),
        run_original_id: vec![original_id],
        face_id: (0..m.triangles.len() as u64).collect(),
        ..MeshGL64::default()
    }
}

fn status_error(status: MfError, operand: Option<u8>) -> KernelError {
    match (status, operand) {
        (MfError::Cancelled, _) => KernelError::Cancelled,
        (MfError::NotClosed, Some(operand)) => KernelError::NotClosed { operand },
        (other, Some(operand)) => KernelError::Internal(format!("operand {operand}: {other:?}")),
        (other, None) => KernelError::Internal(format!("{other:?}")),
    }
}

fn import(m: &KernelMesh, operand: u8, original_id: u32) -> Result<Manifold, KernelError> {
    let manifold = Manifold::from_mesh_gl64_robust(&to_mesh_gl(m, original_id));
    match manifold.status() {
        MfError::NoError => Ok(manifold),
        status => Err(status_error(status, Some(operand))),
    }
}

fn cancelled(ctx: &RunContext) -> Result<(), KernelError> {
    ctx.checkpoint().map_err(|_| KernelError::Cancelled)
}

impl Kernel for ManifoldRobust {
    fn boolean(
        &self,
        a: &KernelMesh,
        b: &KernelMesh,
        op: BoolOp,
        ctx: &RunContext,
    ) -> Result<KernelResult, KernelError> {
        cancelled(ctx)?;
        check_operand(a, 0)?;
        check_operand(b, 1)?;
        let (token, guard) = bridge(ctx);
        guard.report("boolean: importing", 0.0);

        let first_id = Manifold::reserve_ids(2);
        let ids = [first_id, first_id + 1];
        // The import isn't cancellable inside manifold-rust; check between.
        let ma = import(a, 0, ids[0])?;
        cancelled(ctx)?;
        let mb = import(b, 1, ids[1])?;
        cancelled(ctx)?;
        guard.report("boolean: importing", IMPORT_END);

        let op = match op {
            BoolOp::Union => OpType::Add,
            BoolOp::Subtract => OpType::Subtract,
            BoolOp::Intersect => OpType::Intersect,
        };
        let result = ma.boolean_with_engine_rule_and_progress(
            &mb,
            op,
            BooleanEngine::Robust,
            WindingRule::Positive,
            Some(&token),
            Some(guard.reporter()),
        );
        // A run the user cancelled reports Cancelled even if the kernel
        // finished first.
        cancelled(ctx)?;
        match result.status() {
            MfError::NoError => {}
            status => return Err(status_error(status, None)),
        }
        guard.report("boolean: exporting", ENGINE_END);
        let gl = result.get_mesh_gl64(-1);
        let out = from_mesh_gl(&gl, ids, [a, b])?;
        cancelled(ctx)?;
        guard.report("boolean: exporting", 1.0);
        Ok(out)
    }

    fn id(&self) -> &'static str {
        "manifold-rust=0.19.2/robust"
    }
}

fn from_mesh_gl(
    gl: &MeshGL64,
    ids: [u32; 2],
    operands: [&KernelMesh; 2],
) -> Result<KernelResult, KernelError> {
    let stride = gl.num_prop as usize;
    if stride < 3
        || !gl.vert_properties.len().is_multiple_of(stride)
        || !gl.tri_verts.len().is_multiple_of(3)
    {
        return Err(KernelError::Internal(format!(
            "kernel output is malformed: {} properties at stride {stride}, {} indices",
            gl.vert_properties.len(),
            gl.tri_verts.len()
        )));
    }
    let positions: Vec<[f64; 3]> = gl
        .vert_properties
        .chunks_exact(stride)
        .map(|p| [p[0], p[1], p[2]])
        .collect();
    let n = positions.len() as u64;
    let mut triangles = Vec::with_capacity(gl.tri_verts.len() / 3);
    for t in gl.tri_verts.as_chunks::<3>().0 {
        if t.iter().any(|&v| v >= n) {
            return Err(KernelError::Internal(
                "kernel output names a vertex that doesn't exist".into(),
            ));
        }
        triangles.push([t[0] as u32, t[1] as u32, t[2] as u32]);
    }
    if gl.face_id.len() != triangles.len() {
        return Err(KernelError::Internal(format!(
            "kernel output has {} face IDs for {} triangles",
            gl.face_id.len(),
            triangles.len()
        )));
    }
    // Runs: run r covers triangles run_index[r]/3 .. run_index[r+1]/3.
    let runs = gl.run_original_id.len();
    let bounds = |r: usize| -> usize {
        gl.run_index
            .get(r)
            .map_or(gl.tri_verts.len(), |&i| i as usize)
            / 3
    };
    let mut origin = vec![None; triangles.len()];
    for r in 0..runs {
        let operand = match ids.iter().position(|&id| id == gl.run_original_id[r]) {
            Some(i) => i,
            None => {
                return Err(KernelError::Internal(format!(
                    "kernel output run {r} names mesh {} which is neither operand",
                    gl.run_original_id[r]
                )));
            }
        };
        let (start, end) = (bounds(r), bounds(r + 1).min(triangles.len()));
        for (t, slot) in origin.iter_mut().enumerate().take(end).skip(start) {
            let face = gl.face_id[t] as usize;
            let label = operands[operand].face_ids.get(face).ok_or_else(|| {
                KernelError::Internal(format!(
                    "kernel output triangle {t} names face {face} of operand {operand}, which has {}",
                    operands[operand].face_ids.len()
                ))
            })?;
            *slot = Some(*label);
        }
    }
    let origin = origin
        .into_iter()
        .enumerate()
        .map(|(t, o)| {
            o.ok_or_else(|| {
                KernelError::Internal(format!("kernel output triangle {t} is in no run"))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(KernelResult {
        positions,
        triangles,
        origin,
    })
}
