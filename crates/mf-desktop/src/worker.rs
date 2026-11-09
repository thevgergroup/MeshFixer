//! Background work (MF-RT-001, MF-RT-002, MF-RT-004). Every O(n) step runs
//! off the UI thread and reports back over a channel, on one of three pools
//! that share the thread budget:
//!
//! - **jobs**: the running import, operation, export or replay (one at a
//!   time). Nothing else ever runs there: rayon lets a thread waiting in a
//!   join run any queued job of its own pool, so a fill sharing a pool with
//!   a 5M render-vertex build could only stop once that build finished.
//! - **background**: analysis, render meshes and overlays.
//! - **lane** (2 threads): picking and hover, so a click never waits.
//!
//! GPU buffers are created and filled here too, never on the UI thread: a
//! 5M-triangle mesh is hundreds of megabytes (wgpu devices are
//! `Send + Sync`).

use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use glam::DMat4;
use mf_command::{CommandError, Completed, ObjectId, Prepared};
use mf_geometry::{Aabb, Diagnostics, Mesh};
use mf_io::{Format, ImportedModel, IoError};
use mf_render::{
    Bvh, FaceStates, GpuLines, GpuMesh, UploadStats, edge_line_vertices, loop_line_vertices,
};
use mf_runtime::{CancelToken, Cancelled, JobPool, PoolChoice, RunContext};
use mf_selection::SelectionKind;

use crate::defects::{Defect, Defects};

/// What the 3D view needs to draw one mesh revision, built on a worker:
/// its GPU buffers, plus the bounds and local origin the app frames and
/// places overlays with. No CPU copy of the vertices is kept.
pub struct RenderMesh {
    /// The revision the buffers were built for: a later revision's build
    /// reuses every GPU chunk it shares with this one (MF-RENDER-003).
    pub mesh: Mesh,
    /// Object-space bounds; `None` for an empty mesh.
    pub bounds: Option<Aabb>,
    /// The local origin vertices (and defect overlays) are relative to.
    pub origin: glam::DVec3,
    pub gpu: GpuMesh,
    /// The face states last asked for (by the UI thread), so a change is
    /// asked for once.
    requested_states: Mutex<Option<StateInputs>>,
    /// The newest state request written (by workers): an older one that
    /// finishes later is dropped.
    written_states: Mutex<u64>,
}

/// GPU buffers are at most this big, each uploaded before the next (see
/// [`GpuMesh::from_vertices_paced`]): a UI frame waits for at most one
/// chunk's copy. Per-chunk buffers are also what partial re-upload builds
/// on (MF-RENDER-003).
pub const UPLOAD_CHUNK_BYTES: u64 = 16 << 20;

/// The device and queue workers create and fill GPU buffers with.
#[derive(Clone)]
pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    primer: Arc<std::sync::Mutex<Primer>>,
    /// Mesh geometry (surface and outline bytes) uploaded by render-mesh
    /// builds through this and its siblings (diagnostics and tests).
    geometry_uploaded: Arc<std::sync::atomic::AtomicU64>,
}

impl Gpu {
    /// Another handle on the same device, with its own primer (so two
    /// pools can prime at once), counting into the same totals.
    pub fn sibling(&self) -> Gpu {
        Gpu {
            geometry_uploaded: self.geometry_uploaded.clone(),
            ..Gpu::new(self.device.clone(), self.queue.clone())
        }
    }

    /// Mesh geometry bytes uploaded by render-mesh builds so far.
    pub fn geometry_uploaded(&self) -> u64 {
        self.geometry_uploaded
            .load(std::sync::atomic::Ordering::Acquire)
    }

    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> Gpu {
        let primer = Primer {
            renderer: mf_render::MeshRenderer::new(
                &device,
                mf_render::RenderTarget::format(crate::viewport::MSAA_SAMPLES),
                crate::theme::palette(),
            ),
            target: mf_render::RenderTarget::new(&device, 4, 4, crate::viewport::MSAA_SAMPLES)
                .expect("a 4×4 target fits"),
        };
        Gpu {
            device,
            queue,
            primer: Arc::new(std::sync::Mutex::new(primer)),
            geometry_uploaded: Arc::default(),
        }
    }

    /// Draws `mesh` once into a 4×4 target and waits for it. The first
    /// draw of a freshly uploaded 5M-triangle mesh costs the GPU ~20 ms
    /// more than later ones (measured on Metal); paid here, on a worker, it
    /// isn't paid in the UI frame that first shows the mesh (MF-RT-001).
    fn prime(&self, mesh: &GpuMesh, bounds: &Aabb) {
        let mut primer = self.primer.lock().unwrap_or_else(|p| p.into_inner());
        let Primer { renderer, target } = &mut *primer;
        let camera = mf_render::OrbitCamera::fit(bounds);
        let items = [mf_render::DrawItem {
            mesh,
            model: DMat4::IDENTITY,
            inactive: false,
            shown_states: u32::from(FaceStates::ALL),
        }];
        target.draw(
            &self.device,
            &self.queue,
            renderer,
            Some(&camera),
            &items,
            true,
            &[],
        );
        let submission = self.queue.submit([]);
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: None,
            })
            .expect("the GPU device was lost while preparing a mesh");
    }
}

/// A renderer and tiny target for [`Gpu::prime`].
struct Primer {
    renderer: mf_render::MeshRenderer,
    target: mf_render::RenderTarget,
}

impl RenderMesh {
    /// O(n) and a GPU upload: call it on a worker. `skip` leaves out the
    /// boundary outlines of those loops (by ID). `previous` is what the
    /// view shows for this object now: every GPU chunk still valid for
    /// `mesh` is shared with it rather than uploaded again, and only the
    /// changed chunks are generated and uploaded, one at a time (see
    /// [`GpuMesh::from_previous`]). Reuse is decided against `previous`'s
    /// own revision, so it is right whatever the history between the two
    /// (undo, redo, another branch).
    pub fn build(
        gpu: &Gpu,
        mesh: &Mesh,
        skip: &[u32],
        previous: Option<&RenderMesh>,
    ) -> RenderMesh {
        let bounds = mesh.bounding_box();
        let origin = mf_render::render_origin(previous.map(|p| p.origin), bounds);
        let render = RenderMesh {
            mesh: mesh.clone(),
            bounds,
            origin,
            gpu: GpuMesh::from_previous(
                &gpu.device,
                &gpu.queue,
                previous.map(|p| (&p.mesh, &p.gpu)),
                mesh,
                origin,
                skip,
                UPLOAD_CHUNK_BYTES,
            ),
            requested_states: Mutex::new(None),
            written_states: Mutex::new(0),
        };
        let up = render.gpu.last_upload();
        gpu.geometry_uploaded.fetch_add(
            up.surface_bytes + up.line_bytes,
            std::sync::atomic::Ordering::AcqRel,
        );
        if let Some(bb) = &bounds {
            gpu.prime(&render.gpu, bb);
        }
        render
    }
}

impl RenderMesh {
    /// True (and remembered) if `inputs` aren't the face states last asked
    /// for. O(selection): the UI thread asks every frame.
    pub fn wants_states(&self, inputs: &StateInputs) -> bool {
        let mut requested = self
            .requested_states
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if requested.as_ref() == Some(inputs) {
            return false;
        }
        *requested = Some(inputs.clone());
        true
    }

    /// Forget that `inputs` were asked for (their write was superseded
    /// before it happened), so they're asked for again if still wanted.
    fn forget_states(&self, inputs: &StateInputs) {
        let mut requested = self
            .requested_states
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if requested.as_ref() == Some(inputs) {
            *requested = None;
        }
    }

    /// Write `states` (request number `seq`) unless a newer request was
    /// written already. Only changed chunks are written; no geometry.
    pub fn write_states(&self, gpu: &Gpu, states: &FaceStates, seq: u64) -> Option<UploadStats> {
        let mut written = self
            .written_states
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if seq < *written {
            return None;
        }
        *written = seq;
        Some(self.gpu.set_face_states(&gpu.queue, states))
    }
}

/// What the face states of the active object's revision are made from
/// (see [`face_states`]). Small to compare, so the UI thread can tell when
/// they change; the O(n) part is done on a worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateInputs {
    /// The selection's faces (a loop or edge selection tints no face).
    pub selected: Option<OverlayTarget>,
    /// What the pointer is over (never the selection itself).
    pub hover: Option<OverlayTarget>,
    /// Faces made by the last few commands.
    pub recent: Arc<[u32]>,
    /// Faces at a non-manifold edge (from the analysis).
    pub tangled: Arc<[u32]>,
}

/// The face states of `mesh` for `inputs`: selection, hover, loose pieces
/// (every piece but the main body, as the tree lists them), faces at a non-manifold edge, recent repairs. O(the flagged
/// faces and the face chunks they're in), never a pass over the whole
/// mesh, and on a worker (MF-RT-001).
pub fn face_states(
    mesh: &Mesh,
    inputs: &StateInputs,
    ctx: &RunContext,
) -> Result<FaceStates, Cancelled> {
    let topo = mesh.topology_with(ctx)?;
    let mut flagged: Vec<(u32, u8)> = Vec::new();
    let mut set = |faces: &[u32], bit: u8| flagged.extend(faces.iter().map(|&f| (f, bit)));
    // Components are ordered largest first: the first is the main body.
    for c in topo.components.iter().skip(1) {
        set(c, FaceStates::SMALL_COMPONENT);
    }
    set(&inputs.tangled, FaceStates::NON_MANIFOLD_ADJ);
    set(&inputs.recent, FaceStates::RECENT_REPAIR);
    for (target, bit) in [
        (&inputs.selected, FaceStates::SELECTED),
        (&inputs.hover, FaceStates::HOVER),
    ] {
        match target {
            Some(OverlayTarget::Components(ids)) => {
                for &c in ids {
                    if let Some(faces) = topo.components.get(c as usize) {
                        set(faces, bit);
                    }
                }
            }
            Some(OverlayTarget::Faces(ids)) => set(ids, bit),
            Some(OverlayTarget::Loops(_) | OverlayTarget::Edges(_)) | None => {}
        }
    }
    ctx.checkpoint()?;
    Ok(FaceStates::from_flagged(mesh, &mut flagged))
}

/// Analysis results for one mesh revision.
pub struct Analysis {
    pub diagnostics: Diagnostics,
    pub render: Arc<RenderMesh>,
    /// The object transform the measurements were made with.
    pub transform: [f64; 12],
    /// The tree's defect categories (docs/design/visual-language.md),
    /// shared so the panels never copy them per frame.
    pub defects: Arc<Defects>,
    /// Every face at a non-manifold edge, sorted (tinted through the face
    /// states).
    pub tangled_faces: Arc<[u32]>,
    /// Defect overlays in the 3D view, relative to `vertices.origin`.
    pub layers: DefectLayers,
}

/// Line overlays for the defects the 3D view colours (theme.rs): cracks
/// and tangled edges. Holes are the mesh's own boundary outlines; small
/// loose pieces and the faces at a tangled edge are face states.
pub struct DefectLayers {
    pub cracks: Arc<GpuLines>,
    pub tangled: Arc<GpuLines>,
}

/// What an overlay outlines or tints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayTarget {
    Loops(Vec<u32>),
    Components(Vec<u32>),
    Faces(Vec<u32>),
    Edges(Vec<[u32; 2]>),
}

/// What an overlay is for (each purpose has one slot in the app).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum OverlayPurpose {
    Selection,
    Hover,
}

/// An overlay's outline (vertices relative to `origin`; empty for faces
/// and pieces, which are tinted through the face states instead), bounds
/// and outward direction (object space; `None` when the faces around it
/// face every way, like a closed piece).
pub struct Overlay {
    /// The local origin its vertices are relative to: its own, since the
    /// render mesh it's drawn over may keep an earlier revision's origin.
    pub origin: glam::DVec3,
    pub lines: Arc<GpuLines>,
    pub bounds: Option<Aabb>,
    pub normal: Option<glam::DVec3>,
}

pub enum Message {
    Imported(Result<ImportedModel, IoError>),
    Completed(Box<Result<Completed, CommandError>>),
    Analyzed {
        mesh: Mesh,
        analysis: Box<Analysis>,
    },
    /// The render mesh of an inactive object (MF-ISSUE-042). The active
    /// object's comes from `Analyzed` instead.
    ObjectVertices {
        object: ObjectId,
        mesh: Mesh,
        render: Arc<RenderMesh>,
    },
    Exported(Result<PathBuf, IoError>),
    Replayed(Result<Vec<String>, String>),
    /// An overlay built for `target` on `object`'s revision `mesh`.
    Overlay {
        purpose: OverlayPurpose,
        object: ObjectId,
        mesh: Mesh,
        target: OverlayTarget,
        overlay: Overlay,
    },
    /// What a click picked (see [`Workers::pick`]), for the revision it was
    /// made on. `mesh` is `None` only when the click's `active` object had
    /// no mesh to pick against in the first place (for example a non-mesh
    /// geometry kind); a miss against a real mesh still carries it, so a
    /// plain click that hits nothing can clear that object's selection.
    Picked {
        object: ObjectId,
        mesh: Option<Mesh>,
        picked: Option<(SelectionKind, Vec<u32>)>,
        add: bool,
    },
    /// What the pointer is over in the 3D view (see [`Workers::hover`]),
    /// for hover request number `seq`.
    Hovered {
        seq: u64,
        hover: Option<ViewHover>,
    },
    /// `object`'s revision `mesh` has its picking BVH.
    BvhBuilt {
        object: ObjectId,
        mesh: Mesh,
        bvh: Arc<Bvh>,
    },
    /// Face states were written to a render mesh: the view redraws.
    StatesWritten,
    /// The analysis of `mesh` at `transform` panicked.
    AnalysisFailed {
        mesh: Mesh,
        transform: [f64; 12],
        message: String,
    },
    /// A worker panicked. `job` is true when it was the running job (the
    /// one the status bar shows), which is then over.
    Failed {
        what: &'static str,
        job: bool,
        message: String,
    },
}

/// Threads in the picking/hover lane: enough that a hover build never
/// queues behind a click's ray cast.
const LANE_THREADS: usize = 2;

/// How the thread budget is split (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolSizes {
    /// The budget: requested or default, cores, and why.
    pub budget: PoolChoice,
    pub jobs: usize,
    pub background: usize,
}

impl PoolSizes {
    /// A third of the budget (at least one thread) for background work,
    /// the rest (at least one) for jobs. A budget of one thread still gets
    /// one of each.
    pub fn split(budget: PoolChoice) -> PoolSizes {
        let background = (budget.threads / 3).max(1);
        PoolSizes {
            budget,
            jobs: budget.threads.saturating_sub(background).max(1),
            background,
        }
    }
}

/// The app's pools (see the module docs).
pub struct Workers {
    tx: Sender<Message>,
    repaint: eframe::egui::Context,
    /// For building GPU buffers on workers.
    gpu: Gpu,
    jobs: JobPool,
    background: Arc<JobPool>,
    lane: JobPool,
    /// How the pools were sized, for diagnostics.
    pub sizes: PoolSizes,
    /// The latest pick and hover build: a newer one cancels the older, so
    /// the lane only ever works on what the pointer is doing now.
    latest_pick: Option<CancelToken>,
    latest_hover: Option<CancelToken>,
    latest_states: Option<CancelToken>,
}

/// The message for a worker that panicked with `payload`.
pub fn failed(what: &'static str, job: bool, payload: Box<dyn std::any::Any + Send>) -> Message {
    let message = match payload.downcast::<String>() {
        Ok(s) => *s,
        Err(payload) => match payload.downcast::<&'static str>() {
            Ok(s) => (*s).to_owned(),
            Err(_) => "unknown panic".to_owned(),
        },
    };
    Message::Failed { what, job, message }
}

/// Run `f` on `pool` and send what it returns (`None`: nothing to say, as
/// for superseded work). A panic becomes a [`Message::Failed`]: a worker
/// must never take the app down, nor leave it waiting for an answer that
/// will never come.
fn spawn_on(
    pool: &JobPool,
    tx: &Sender<Message>,
    repaint: &eframe::egui::Context,
    what: &'static str,
    job: bool,
    f: impl FnOnce() -> Option<Message> + Send + 'static,
) {
    let tx = tx.clone();
    let repaint = repaint.clone();
    pool.spawn(move || {
        let msg = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
            Ok(msg) => msg,
            Err(payload) => Some(failed(what, job, payload)),
        };
        if let Some(msg) = msg {
            // allow-swallow: the receiver only disappears while the app is closing
            let _ = tx.send(msg);
            repaint.request_repaint();
        }
    });
}

/// A fresh context for a lane job, cancelling the one it supersedes.
fn supersede(latest: &mut Option<CancelToken>) -> RunContext {
    if let Some(old) = latest.take() {
        old.cancel();
    }
    let ctx = RunContext::none();
    *latest = Some(ctx.cancel.clone());
    ctx
}

impl Workers {
    /// `threads` is the whole budget (`None`: all cores but one), split
    /// between the jobs and background pools ([`PoolSizes::split`]).
    pub fn new(
        tx: Sender<Message>,
        repaint: eframe::egui::Context,
        threads: Option<usize>,
        gpu: Gpu,
    ) -> Workers {
        let sizes = PoolSizes::split(PoolChoice::for_request(threads));
        let (jobs, _) = JobPool::named("mf-jobs", Some(sizes.jobs));
        let (background, _) = JobPool::named("mf-bg", Some(sizes.background));
        let (lane, _) = JobPool::named("mf-lane", Some(LANE_THREADS));
        Workers {
            tx,
            repaint,
            gpu,
            jobs,
            background: Arc::new(background),
            lane,
            sizes,
            latest_pick: None,
            latest_hover: None,
            latest_states: None,
        }
    }

    /// The background pool, shared with the analysis worker.
    pub fn background(&self) -> Arc<JobPool> {
        self.background.clone()
    }

    /// The running job, on the jobs pool.
    fn spawn_job(&self, what: &'static str, f: impl FnOnce() -> Message + Send + 'static) {
        spawn_on(&self.jobs, &self.tx, &self.repaint, what, true, move || {
            Some(f())
        });
    }

    fn spawn_background(&self, what: &'static str, f: impl FnOnce() -> Message + Send + 'static) {
        spawn_on(
            &self.background,
            &self.tx,
            &self.repaint,
            what,
            false,
            move || Some(f()),
        );
    }

    /// Drop `value` on the background pool: freeing a big revision's
    /// vertex data and GPU buffers takes long enough to show in a frame.
    pub fn discard<T: Send + 'static>(&self, value: T) {
        self.background.spawn(move || drop(value));
    }

    pub fn import(&self, path: PathBuf, ctx: RunContext) {
        self.spawn_job("import", move || {
            Message::Imported(mf_io::read_model_with(&path, &ctx))
        });
    }

    pub fn run(&self, prepared: Prepared, ctx: RunContext) {
        self.spawn_job("operation", move || {
            Message::Completed(Box::new(prepared.run(&ctx)))
        });
    }

    /// Build an overlay: O(target) to O(n) (a large component, or finding a
    /// loop's vertices), so never on the UI thread (MF-RT-001). Hover
    /// overlays go to the lane, superseding the previous hover. `origin`
    /// is the local origin of the render mesh the overlay is drawn over:
    /// its lines must be relative to the very same point as the mesh's own
    /// outlines, or their `f32` positions round differently and the lines
    /// z-fight (fix round 1, I1).
    pub fn build_overlay(
        &mut self,
        purpose: OverlayPurpose,
        object: ObjectId,
        mesh: Mesh,
        target: OverlayTarget,
        origin: glam::DVec3,
    ) {
        let gpu = self.gpu.clone();
        if purpose == OverlayPurpose::Hover {
            let ctx = supersede(&mut self.latest_hover);
            spawn_on(
                &self.lane,
                &self.tx,
                &self.repaint,
                "hover outline",
                false,
                move || {
                    // allow-swallow: a superseded hover has nothing to show
                    build_overlay(purpose, object, mesh, target, origin, &ctx, &gpu).ok()
                },
            );
        } else {
            self.spawn_background("overlay", move || {
                build_overlay(
                    purpose,
                    object,
                    mesh,
                    target,
                    origin,
                    &RunContext::none(),
                    &gpu,
                )
                .expect("an uncancelled overlay build completes")
            });
        }
    }

    /// Build `render`'s face states for `inputs` (O(n)) and write the
    /// changed chunks (request number `seq`), on the lane, superseding the
    /// previous request: a selection or hover change uploads no geometry
    /// (MF-RENDER-003), and the UI thread does none of it (MF-RT-001).
    pub fn write_states(&mut self, render: Arc<RenderMesh>, inputs: StateInputs, seq: u64) {
        let ctx = supersede(&mut self.latest_states);
        let gpu = self.gpu.clone();
        spawn_on(
            &self.lane,
            &self.tx,
            &self.repaint,
            "face states",
            false,
            move || {
                let Ok(states) = face_states(&render.mesh, &inputs, &ctx) else {
                    // Superseded (perhaps by a request for another object's
                    // mesh): this one is asked for again if still wanted.
                    render.forget_states(&inputs);
                    return None;
                };
                render.write_states(&gpu, &states, seq)?;
                Some(Message::StatesWritten)
            },
        );
    }

    /// Render vertices only, for an object that isn't active
    /// (MF-ISSUE-042): it still needs to be drawn at its transform, but not
    /// analysed. `previous` is what the view draws for it now (see
    /// [`RenderMesh::build`]).
    pub fn build_vertices(&self, object: ObjectId, mesh: Mesh, previous: Option<Arc<RenderMesh>>) {
        let gpu = self.gpu.clone();
        self.spawn_background("render mesh", move || {
            let render = Arc::new(RenderMesh::build(&gpu, &mesh, &[], previous.as_deref()));
            Message::ObjectVertices {
                object,
                mesh,
                render,
            }
        });
    }

    /// Replay a command log (it re-imports source files).
    pub fn replay(&self, dir: PathBuf) {
        self.spawn_job("replay", move || {
            Message::Replayed(
                mf_command::replay(&dir)
                    .map(|report| {
                        let mut lines: Vec<String> = report
                            .results
                            .iter()
                            .map(|(id, v)| format!("command {}: {v:?}", id.0))
                            .collect();
                        lines.push(if report.all_verified() {
                            format!("All {} command(s) verified.", report.results.len())
                        } else {
                            "Replay did NOT reproduce the log.".into()
                        });
                        lines
                    })
                    .map_err(|e| e.to_string()),
            )
        });
    }

    /// Pick on the lane, across every object ([`pick_targets`]: BVH ray
    /// casts, or brute force for a target whose BVH isn't built yet). All
    /// the work (waiting for topology, ray casts) happens there, never on
    /// the UI thread (MF-RT-001). `active` is reported when the click hits
    /// nothing, so a miss still clears the right object's selection; its
    /// mesh comes from `targets` if it has one there (an active object with
    /// no mesh, such as a non-mesh geometry kind, reports no mesh rather
    /// than a fabricated one). A newer click supersedes this one.
    #[allow(clippy::too_many_arguments)]
    pub fn pick(
        &mut self,
        targets: Vec<PickTarget>,
        active: ObjectId,
        cam: mf_render::OrbitCamera,
        origin: glam::DVec3,
        size: (f64, f64),
        px: (f64, f64),
        add: bool,
    ) {
        let ctx = supersede(&mut self.latest_pick);
        spawn_on(
            &self.lane,
            &self.tx,
            &self.repaint,
            "pick",
            false,
            move || {
                // The analysis may be computing a target's topology right now:
                // wait for it cancellably, so a newer click isn't kept waiting.
                for t in &targets {
                    // allow-swallow: a superseded click has nothing to report
                    t.mesh.topology_with(&ctx).ok()?;
                }
                // allow-swallow: a superseded click has nothing to report
                let picked = pick_targets(&targets, &cam, origin, size, px, &ctx).ok()?;
                Some(match picked {
                    Some(hit) => Message::Picked {
                        object: hit.target.object,
                        mesh: Some(hit.target.mesh.clone()),
                        picked: Some(hit.selection()),
                        add,
                    },
                    None => Message::Picked {
                        object: active,
                        mesh: targets
                            .iter()
                            .find(|t| t.object == active)
                            .map(|t| t.mesh.clone()),
                        picked: None,
                        add,
                    },
                })
            },
        );
    }

    /// What the pointer is over in the 3D view, on the lane: the same pick
    /// as a click ([`pick_targets`]), reported as hover number `seq` when
    /// it lands on one of the active object's defects (a hole, a loose
    /// piece, a drawn tangled edge), as `None` otherwise. The app keeps at
    /// most one in flight and drops a result a newer pointer position has
    /// made stale. The app only asks once every target has its BVH and
    /// topology; one without topology here (released since) makes this a
    /// miss, rather than an O(n) topology build on the lane.
    #[allow(clippy::too_many_arguments)]
    pub fn hover(
        &self,
        targets: Vec<PickTarget>,
        cam: mf_render::OrbitCamera,
        origin: glam::DVec3,
        size: (f64, f64),
        px: (f64, f64),
        seq: u64,
    ) {
        spawn_on(
            &self.lane,
            &self.tx,
            &self.repaint,
            "hover",
            false,
            move || {
                if targets.iter().any(|t| !t.mesh.has_cached_topology()) {
                    return Some(Message::Hovered { seq, hover: None });
                }
                // Nothing supersedes a hover pick (one is in flight at a
                // time), and with every BVH built it does no O(n) work.
                let picked = pick_targets(&targets, &cam, origin, size, px, &RunContext::none())
                    .expect("a context that is never cancelled can't be cancelled");
                let hover = picked.and_then(|hit| {
                    let defect = hit.defect()?;
                    Some(ViewHover {
                        object: hit.target.object,
                        mesh: hit.target.mesh.clone(),
                        target: hit.overlay_target(),
                        defect,
                    })
                });
                Some(Message::Hovered { seq, hover })
            },
        );
    }

    /// Export on the pool; cancelling leaves no file behind (see
    /// [`mf_io::write_path_with`]).
    pub fn export(&self, job: ExportJob, path: PathBuf, ctx: RunContext) {
        self.spawn_job("export", move || {
            let result = match &job {
                ExportJob::Mesh(mesh, format) => mf_io::write_path_with(mesh, &path, *format, &ctx),
                ExportJob::Model(model) => mf_io::write_model_path_with(model, &path, &ctx),
            };
            Message::Exported(result.map(|()| path))
        });
    }
}

/// The largest hole the fill operation can take (its `max_loop_edges` limit).
fn fill_cap() -> usize {
    let spec = mf_spec::builtin::get("mesh.fill_holes").expect("fill_holes is built in");
    spec.parameters
        .iter()
        .find_map(|p| match (&p.name[..], &p.kind) {
            ("max_loop_edges", mf_spec::ParamKind::Integer { max: Some(max), .. }) => {
                Some(*max as usize)
            }
            _ => None,
        })
        .expect("fill_holes declares a maximum max_loop_edges")
}

/// One revision to analyse.
#[derive(Clone)]
pub struct AnalysisRequest {
    pub mesh: Mesh,
    /// The object transform to measure with.
    pub transform: [f64; 12],
    /// What the view draws for the object now, whose GPU chunks the new
    /// render mesh reuses where they're still valid (see
    /// [`RenderMesh::build`]).
    pub previous: Option<Arc<RenderMesh>>,
    /// The face states to give the new render mesh before it's shown (and
    /// their request number), so it never appears without its tints.
    pub states: Option<(StateInputs, u64)>,
}

/// The analysis a [`crate::Settings`] can swap in (tests inject failures).
pub type AnalyzeFn = fn(AnalysisRequest, &RunContext, &Gpu) -> Result<Message, Cancelled>;

/// Diagnostics, defects and render vertices for one revision (O(n), so off
/// the UI thread). Measurements are made after the object's `transform`.
/// Cancelled when a newer revision is submitted (see analysis.rs).
pub fn analyze(
    request: AnalysisRequest,
    ctx: &RunContext,
    gpu: &Gpu,
) -> Result<Message, Cancelled> {
    let AnalysisRequest {
        mesh,
        transform,
        previous,
        states,
    } = request;
    let diagnostics = mf_geometry::inspect_with(&mesh, ctx)?;
    ctx.checkpoint()?;
    let defects = crate::defects::classify(&mesh, &diagnostics, &transform, fill_cap());
    let crack_ids: Vec<u32> = defects
        .cracks
        .iter()
        .filter_map(|d| d.selection().map(|(_, id)| id))
        .collect();
    ctx.checkpoint()?;
    // Cracks get only the crack colour: no hole outline under them.
    let render = Arc::new(RenderMesh::build(
        gpu,
        &mesh,
        &crack_ids,
        previous.as_deref(),
    ));
    // Let go of it here, on the worker: if the app already has, freeing
    // its unshared buffers stays out of the UI thread.
    drop(previous);
    ctx.checkpoint()?;
    let origin = render.origin;
    let tangled: Vec<[u32; 2]> = defects
        .tangled
        .iter()
        .filter_map(|d| match d {
            Defect::Tangled { edge, .. } => Some(*edge),
            _ => None,
        })
        .collect();
    let mut tangled_faces: Vec<u32> = defects
        .tangled
        .iter()
        .flat_map(|d| match d {
            Defect::Tangled { faces, .. } => faces.as_slice(),
            _ => &[],
        })
        .copied()
        .collect();
    tangled_faces.sort_unstable();
    tangled_faces.dedup();
    let tangled_faces: Arc<[u32]> = tangled_faces.into();
    if let Some((mut inputs, seq)) = states {
        inputs.tangled = tangled_faces.clone();
        let face_states = face_states(&mesh, &inputs, ctx)?;
        render.wants_states(&inputs);
        render.write_states(gpu, &face_states, seq);
    }
    let layers = DefectLayers {
        cracks: Arc::new(GpuLines::upload_paced(
            &gpu.device,
            &gpu.queue,
            &loop_line_vertices(&mesh, origin, &crack_ids),
            UPLOAD_CHUNK_BYTES,
        )),
        tangled: Arc::new(GpuLines::upload_paced(
            &gpu.device,
            &gpu.queue,
            &edge_line_vertices(&mesh, origin, &tangled),
            UPLOAD_CHUNK_BYTES,
        )),
    };
    let analysis = Analysis {
        diagnostics,
        render,
        transform,
        defects: Arc::new(defects),
        tangled_faces,
        layers,
    };
    Ok(Message::Analyzed {
        mesh,
        analysis: Box::new(analysis),
    })
}

/// An overlay for `target` on `mesh` (see [`Workers::build_overlay`]).
fn build_overlay(
    purpose: OverlayPurpose,
    object: ObjectId,
    mesh: Mesh,
    target: OverlayTarget,
    origin: glam::DVec3,
    ctx: &RunContext,
    gpu: &Gpu,
) -> Result<Message, Cancelled> {
    {
        let topo = mesh.topology_with(ctx)?;
        // The target's vertices, and its outline as vertex pairs.
        let (points, outline): (Vec<u32>, Vec<[u32; 2]>) = match &target {
            OverlayTarget::Loops(ids) => {
                let mut points = Vec::new();
                let mut outline = Vec::new();
                for l in topo.boundary_loops.iter().filter(|l| ids.contains(&l.id)) {
                    points.extend(l.vertices.iter().copied());
                    let n = l.vertices.len();
                    outline.extend((0..n).map(|k| [l.vertices[k], l.vertices[(k + 1) % n]]));
                }
                (points, outline)
            }
            OverlayTarget::Components(ids) => {
                let faces: Vec<u32> = ids
                    .iter()
                    .filter_map(|&c| topo.components.get(c as usize))
                    .flatten()
                    .copied()
                    .collect();
                (face_corners(&mesh, &faces).collect(), Vec::new())
            }
            OverlayTarget::Faces(ids) => (face_corners(&mesh, ids).collect(), Vec::new()),
            OverlayTarget::Edges(edges) => {
                (edges.iter().flatten().copied().collect(), edges.clone())
            }
        };
        let lines = match &target {
            OverlayTarget::Loops(ids) => loop_line_vertices(&mesh, origin, ids),
            OverlayTarget::Edges(edges) => edge_line_vertices(&mesh, origin, edges),
            OverlayTarget::Components(_) | OverlayTarget::Faces(_) => Vec::new(),
        };
        // Which way the target faces, for framing it (F): only a selection
        // is framed, and finding a loop's faces is O(n).
        let normal = match (&target, purpose) {
            (_, OverlayPurpose::Hover) => None,
            (OverlayTarget::Loops(_) | OverlayTarget::Edges(_), _) => facing(
                &mesh,
                &faces_on_edges(&mesh, outline.iter().map(|e| (e[0], e[1])).collect()),
            ),
            (OverlayTarget::Components(ids), _) => facing(
                &mesh,
                &ids.iter()
                    .filter_map(|&c| topo.components.get(c as usize))
                    .flatten()
                    .copied()
                    .collect::<Vec<u32>>(),
            ),
            (OverlayTarget::Faces(ids), _) => facing(&mesh, ids),
        };
        let bounds =
            points
                .iter()
                .map(|&v| mesh.corner_position(v))
                .fold(None, |acc: Option<Aabb>, p| {
                    Some(match acc {
                        None => Aabb { min: p, max: p },
                        Some(b) => Aabb {
                            min: [0, 1, 2].map(|k| b.min[k].min(p[k])),
                            max: [0, 1, 2].map(|k| b.max[k].max(p[k])),
                        },
                    })
                });
        Ok(Message::Overlay {
            purpose,
            object,
            mesh: mesh.clone(),
            target,
            overlay: Overlay {
                origin,
                lines: Arc::new(GpuLines::upload_paced(
                    &gpu.device,
                    &gpu.queue,
                    &lines,
                    UPLOAD_CHUNK_BYTES,
                )),
                bounds,
                normal,
            },
        })
    }
}

/// Live faces with an edge among `edges` (either direction). O(n).
fn faces_on_edges(mesh: &Mesh, edges: Vec<(u32, u32)>) -> Vec<u32> {
    let mut keys: Vec<(u32, u32)> = edges
        .into_iter()
        .map(|(a, b)| (a.min(b), a.max(b)))
        .collect();
    keys.sort_unstable();
    mesh.faces()
        .filter(|(_, t)| {
            (0..3).any(|k| {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                keys.binary_search(&(a.min(b), a.max(b))).is_ok()
            })
        })
        .map(|(f, _)| f.0)
        .collect()
}

/// The area-weighted mean normal of `faces`, if they mostly face one way.
fn facing(mesh: &Mesh, faces: &[u32]) -> Option<glam::DVec3> {
    let mut sum = glam::DVec3::ZERO;
    let mut area = 0.0;
    for &f in faces {
        let Some(t) = mesh.face(mf_geometry::FaceId(f)) else {
            continue;
        };
        let [a, b, c] = t.map(|v| glam::DVec3::from(mesh.corner_position(v)));
        let n = (b - a).cross(c - a);
        sum += n;
        area += n.length();
    }
    // Faces that face every way (a closed piece) cancel out.
    (area > 0.0 && sum.length() > 0.2 * area).then(|| sum.normalize())
}

/// The corner vertices of the live faces among `faces`.
fn face_corners<'a>(mesh: &'a Mesh, faces: &'a [u32]) -> impl Iterator<Item = u32> + 'a {
    faces
        .iter()
        .filter_map(|&f| mesh.face(mf_geometry::FaceId(f)))
        .flatten()
}

/// One object a click can land on: its mesh and object-to-world transform
/// (MF-ISSUE-042).
pub struct PickTarget {
    pub object: ObjectId,
    pub mesh: Mesh,
    pub model: DMat4,
    /// Its picking BVH (MF-RENDER-004). `None` while it's still being
    /// built for a new revision: a click then casts by brute force over
    /// every face (explicitly, and only until the BVH arrives); hover
    /// waits for it instead.
    pub bvh: Option<Arc<Bvh>>,
    /// Its defects (the active object, once analysed): what a hover can
    /// name, and its tangled edges.
    pub defects: Option<Arc<Defects>>,
    /// Its tangled edges are drawn, so the pointer can land on one.
    pub tangled: bool,
}

impl PickTarget {
    /// Nearest face hit by a world-space ray, through the BVH when there
    /// is one, else by brute force (see [`PickTarget::bvh`]; ~50 ms at 5M,
    /// cancellable through `ctx`).
    fn ray_pick(
        &self,
        origin: [f64; 3],
        dir: [f64; 3],
        ctx: &RunContext,
    ) -> Result<Option<(mf_geometry::FaceId, f64)>, Cancelled> {
        match &self.bvh {
            Some(bvh) => Ok(bvh.ray_pick(&self.mesh, origin, dir, &self.model)),
            None => mf_render::ray_pick_with(&self.mesh, &self.model, origin, dir, ctx),
        }
    }

    /// The tangled edges drawn on it (and so pickable), with their faces.
    fn tangled_edges(&self) -> Vec<([u32; 2], &[u32])> {
        match (&self.defects, self.tangled) {
            (Some(d), true) => d
                .tangled
                .iter()
                .filter_map(|d| match d {
                    Defect::Tangled { edge, faces } => Some((*edge, faces.as_slice())),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    }
}

/// What the pointer landed on, on one target.
enum Hit<'a> {
    /// A boundary loop's edge (the loop).
    Loop(u32),
    /// A drawn tangled edge, and the faces that share it.
    Tangled([u32; 2], &'a [u32]),
    /// A face of this edge-connected component.
    Component(u32),
}

/// What `pick_targets` found: which target and what on it.
struct PickHit<'a> {
    target: &'a PickTarget,
    hit: Hit<'a>,
}

impl PickHit<'_> {
    /// What a click on it selects.
    fn selection(&self) -> (SelectionKind, Vec<u32>) {
        match self.hit {
            Hit::Loop(id) => (SelectionKind::BoundaryLoop, vec![id]),
            Hit::Tangled(_, faces) => {
                let mut faces = faces.to_vec();
                faces.sort_unstable();
                faces.dedup();
                (SelectionKind::Faces, faces)
            }
            Hit::Component(c) => (SelectionKind::Component, vec![c]),
        }
    }

    /// What a hover over it outlines or tints.
    fn overlay_target(&self) -> OverlayTarget {
        match self.hit {
            Hit::Loop(id) => OverlayTarget::Loops(vec![id]),
            Hit::Tangled(edge, _) => OverlayTarget::Edges(vec![edge]),
            Hit::Component(c) => OverlayTarget::Components(vec![c]),
        }
    }

    /// The defect it is, if its object's defects are known and it is one
    /// (the main body, for one, isn't).
    fn defect(&self) -> Option<Defect> {
        let defects = self.target.defects.as_ref()?;
        match self.hit {
            Hit::Loop(id) => defects.find(SelectionKind::BoundaryLoop, id).cloned(),
            Hit::Component(c) => defects.find(SelectionKind::Component, c).cloned(),
            Hit::Tangled(edge, _) => defects
                .tangled
                .iter()
                .find(|d| matches!(d, Defect::Tangled { edge: e, .. } if *e == edge))
                .cloned(),
        }
    }
}

/// What the pointer is over in the 3D view: one of the active object's
/// defects (see [`Workers::hover`]).
#[derive(Debug, Clone)]
pub struct ViewHover {
    pub object: ObjectId,
    /// The revision it was found on.
    pub mesh: Mesh,
    pub target: OverlayTarget,
    pub defect: Defect,
}

/// The pick under `px` across every object: O(boundary and tangled edges)
/// screen projections, plus BVH ray casts (O(log n) each; brute force for a
/// target without its BVH yet). An edge wins over a face: a boundary
/// loop's edge (the loop is picked), or a drawn tangled edge (its faces
/// are picked, as a face selection). Among edges within tolerance on any
/// object, the nearest one on screen wins, and it must be visible past
/// *every* object's faces (not just its own mesh's), so a click on a front
/// object's face can't select a hidden loop on an object behind it
/// (MF-ISSUE-042). Otherwise the clicked face's component is picked. Pure
/// function (no I/O), so it's directly testable. Every target's topology
/// must be computed (it's cached on the mesh). Cancelled through `ctx`
/// (only the brute-force casts of a target without its BVH check it: the
/// rest is microseconds).
fn pick_targets<'a>(
    targets: &'a [PickTarget],
    cam: &mf_render::OrbitCamera,
    origin: glam::DVec3,
    size: (f64, f64),
    px: (f64, f64),
    ctx: &RunContext,
) -> Result<Option<PickHit<'a>>, Cancelled> {
    const TOLERANCE: f64 = 8.0;
    let eye = cam.eye();
    // (target, what, screen distance, point, endpoints)
    let mut candidates: Vec<(&PickTarget, Hit<'a>, f64, glam::DVec3, [u32; 2])> = Vec::new();
    for t in targets {
        for c in
            mf_render::loop_edge_candidates(&t.mesh, &t.model, cam, origin, size, px, TOLERANCE)
        {
            candidates.push((
                t,
                Hit::Loop(c.loop_id),
                c.screen_distance,
                c.point,
                c.endpoints,
            ));
        }
        let tangled = t.tangled_edges();
        if !tangled.is_empty() {
            let edges: Vec<[u32; 2]> = tangled.iter().map(|(e, _)| *e).collect();
            for c in mf_render::edge_candidates(
                &t.mesh, &t.model, cam, origin, size, px, TOLERANCE, &edges,
            ) {
                let (edge, faces) = tangled[c.index];
                candidates.push((
                    t,
                    Hit::Tangled(edge, faces),
                    c.screen_distance,
                    c.point,
                    c.endpoints,
                ));
            }
        }
    }
    candidates.sort_by(|a, b| a.2.total_cmp(&b.2));
    for (target, hit, _, point, endpoints) in candidates {
        let mut hidden = false;
        for other in targets {
            if mf_render::edge_occluded_with(
                &other.mesh,
                other.bvh.as_deref(),
                &other.model,
                eye,
                point,
                endpoints,
                ctx,
            )? {
                hidden = true;
                break;
            }
        }
        if !hidden {
            return Ok(Some(PickHit { target, hit }));
        }
    }

    let (ray_origin, dir) = cam.ray(origin, size, px);
    let mut best: Option<(&PickTarget, Hit<'a>, f64)> = None;
    for t in targets {
        let Some((f, dist)) = t.ray_pick(ray_origin, dir, ctx)? else {
            continue;
        };
        // Components list their faces sorted: O(components · log n).
        let Some(component) = t
            .mesh
            .topology()
            .components
            .iter()
            .position(|c| c.binary_search(&f.0).is_ok())
        else {
            continue;
        };
        if best.as_ref().is_none_or(|(.., b)| dist < *b) {
            best = Some((t, Hit::Component(component as u32), dist));
        }
    }
    Ok(best.map(|(target, hit, _)| PickHit { target, hit }))
}

/// What to export: one mesh, or a whole 3MF model.
pub enum ExportJob {
    Mesh(Mesh, Format),
    Model(mf_io::threemf::Model),
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // tests may panic on failure
mod tests {
    use super::*;
    use glam::DVec3;
    use mf_fixtures::{cube, cube_with_missing_face};
    use mf_render::{OrbitCamera, screen_position};

    /// Looking almost straight down (a tiny tilt off vertical to avoid the
    /// view-matrix singularity), framing both objects below.
    fn overhead_camera(target: DVec3, distance: f64) -> OrbitCamera {
        OrbitCamera {
            target,
            distance,
            yaw: 0.0,
            pitch: 89f64.to_radians(),
            fov_y: 40f64.to_radians(),
            radius: distance,
        }
    }

    /// A narrow crack is drawn only in the crack colour (fix round 1, P3):
    /// the hole-coloured boundary outline leaves its loop out, and the crack
    /// layer carries it.
    #[test]
    fn cracks_are_not_outlined_in_the_hole_colour() {
        let Ok((device, queue)) = mf_render::offscreen::shared_device() else {
            mf_fixtures::skip::gpu_unavailable("cracks_are_not_outlined_in_the_hole_colour");
            return;
        };
        let plate = mf_fixtures::plate_with_defects();
        let identity = mf_io::threemf::IDENTITY;
        let Ok(Message::Analyzed { analysis, .. }) = analyze(
            AnalysisRequest {
                mesh: plate,
                transform: identity,
                previous: None,
                states: None,
            },
            &RunContext::none(),
            &Gpu::new(device, queue),
        ) else {
            panic!("analyze answers with Analyzed");
        };
        let edges = |list: &[Defect]| -> usize {
            list.iter()
                .map(|d| match d {
                    Defect::Hole { edges, .. }
                    | Defect::OverCap { edges, .. }
                    | Defect::Crack { edges, .. } => *edges,
                    _ => 0,
                })
                .sum()
        };
        assert_eq!(analysis.defects.cracks.len(), 1);
        assert_eq!(
            analysis.render.gpu.line_vertex_count(),
            2 * edges(&analysis.defects.holes),
            "only holes get the hole outline"
        );
        assert_eq!(
            analysis.layers.cracks.vertex_count(),
            2 * edges(&analysis.defects.cracks)
        );
    }

    /// The thread budget is split between the running job and background
    /// work, never adding up to more than the budget (so the default still
    /// leaves a core free) unless the budget is a single thread.
    #[test]
    fn the_thread_budget_is_split_between_jobs_and_background_work() {
        let budget = |threads| PoolChoice {
            threads,
            cores: 12,
            reason: "test",
        };
        let split = |t| {
            let s = PoolSizes::split(budget(t));
            (s.jobs, s.background)
        };
        assert_eq!(split(11), (8, 3));
        assert_eq!(split(3), (2, 1));
        assert_eq!(split(2), (1, 1));
        assert_eq!(split(1), (1, 1));
        for t in 2..64 {
            let (jobs, background) = split(t);
            assert_eq!(jobs + background, t);
        }
    }

    /// A click near a loop edge that another object's solid face hides
    /// must not select that loop (MF-ISSUE-042 review I3): the occlusion
    /// check must cover every target, not just the loop's own mesh.
    #[test]
    fn loop_hidden_by_another_objects_face_is_not_picked() {
        // Back object: a cube missing its top face, open at z=1 over x,y in [0,1].
        let back = cube_with_missing_face();
        let back_target = PickTarget {
            object: ObjectId(1),
            mesh: back.clone(),
            model: DMat4::IDENTITY,
            bvh: None,
            defects: None,
            tangled: false,
        };
        // Front object: a solid cube directly above, z in [2,3], same XY
        // footprint, so viewed from overhead it covers the hole below it.
        let front = cube(1.0);
        let front_target = PickTarget {
            object: ObjectId(2),
            mesh: front,
            model: DMat4::from_translation(DVec3::new(0.0, 0.0, 2.0)),
            bvh: None,
            defects: None,
            tangled: false,
        };
        let targets = vec![back_target, front_target];

        let cam = overhead_camera(DVec3::new(0.5, 0.5, 1.5), 20.0);
        let origin = DVec3::ZERO;
        let size = (800.0, 600.0);

        let topo = back.topology();
        let hole = &topo.boundary_loops[0];
        let a = DVec3::from(back.corner_position(hole.vertices[0]));
        let b = DVec3::from(back.corner_position(hole.vertices[1]));
        let mid = screen_position(&cam, origin, size, ((a + b) * 0.5).to_array())
            .expect("the hole's edge projects on screen");

        let hit = pick_targets(&targets, &cam, origin, size, mid, &RunContext::none()).unwrap();
        match hit {
            Some(h) if h.selection().0 == SelectionKind::BoundaryLoop => {
                panic!(
                    "picked loop {:?} on object {:?}, but the front cube's face hides it",
                    h.selection(),
                    h.target.object
                );
            }
            _ => {}
        }
    }

    /// Without another object in front, the same click does find the loop:
    /// confirms the geometry above is set up to actually land on the hole's
    /// edge, so the occlusion test is about occlusion, not a missed click.
    #[test]
    fn same_loop_is_picked_when_nothing_hides_it() {
        let back = cube_with_missing_face();
        let targets = vec![PickTarget {
            object: ObjectId(1),
            mesh: back.clone(),
            model: DMat4::IDENTITY,
            bvh: None,
            defects: None,
            tangled: false,
        }];
        let cam = overhead_camera(DVec3::new(0.5, 0.5, 1.5), 20.0);
        let origin = DVec3::ZERO;
        let size = (800.0, 600.0);
        let topo = back.topology();
        let hole = &topo.boundary_loops[0];
        let a = DVec3::from(back.corner_position(hole.vertices[0]));
        let b = DVec3::from(back.corner_position(hole.vertices[1]));
        let mid = screen_position(&cam, origin, size, ((a + b) * 0.5).to_array()).unwrap();

        let hit = pick_targets(&targets, &cam, origin, size, mid, &RunContext::none()).unwrap();
        assert!(
            matches!(hit, Some(h) if h.selection() == (SelectionKind::BoundaryLoop, vec![hole.id])),
            "expected the hole's loop with nothing in front of it"
        );
    }
}
