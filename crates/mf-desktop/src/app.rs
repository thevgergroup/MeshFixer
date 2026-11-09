use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::time::Instant;

use eframe::egui;
use mf_command::{CommandError, CommitOutcome, Invocation, ObjectId, RevisionId, Session};
use mf_geometry::{Aabb, Mesh};
use mf_io::{Format, ImportedModel, IoError};
use mf_metrics::FrameTimes;
use mf_render::{
    Bvh, CameraMove, FaceStates, OrbitCamera, SelectionCue, clip_radius, model_matrix,
    screen_position,
};
use mf_runtime::RunContext;
use mf_spec::ParamValue;

use crate::analysis::{AnalysisCache, LatestWorker};
use crate::defects::{Defect, operation_label};
use crate::theme;
use crate::viewport::{Click, NavAction, ObjectView, ViewLayer, ViewOptions, Viewport, ZOOM_STEP};
use crate::worker::{
    self, Analysis, Message, Overlay, OverlayPurpose, OverlayTarget, PickTarget, PoolSizes,
    RenderMesh, StateInputs, ViewHover, Workers,
};
use mf_selection::{Selection, SelectionKind, TranslateError, Translation};

/// "Recent repairs" are the faces created by this many of the active
/// object's latest commands.
pub const RECENT_REPAIR_COMMANDS: usize = 3;

/// The status after a cancelled import or operation.
pub const CANCELLED: &str = "Cancelled: document unchanged";
/// The status after a cancelled export.
pub const CANCELLED_EXPORT: &str = "Cancelled: nothing exported";

/// App settings that are fixed for a run.
#[derive(Debug, Clone, Default)]
pub struct Settings {
    /// Background threads, split between the running job and analysis
    /// (`worker::PoolSizes`); `None` leaves one core free (MF-RT-004).
    pub threads: Option<usize>,
    /// Replaces the analysis of each revision (`None`: the real one). For
    /// tests, which inject failures through it.
    pub analyze: Option<worker::AnalyzeFn>,
}

/// How the running job (the one the status bar shows) can be stopped.
struct RunningJob {
    /// `None`: this kind of job can't be cancelled (replay).
    run: Option<RunContext>,
    cancel_requested: bool,
}

/// An operation offered in Properties for the current target.
#[derive(Debug, Clone, PartialEq)]
pub struct Tool {
    pub op_id: String,
    /// Plain-language name (defects::operation_label).
    pub label: String,
    /// What it runs on: `None` is the whole object.
    pub selection: Option<Selection>,
    /// Parameters the target decides (hidden from the panel): an explicit
    /// target already says which hole or piece, so size limits follow it.
    pub fixed: BTreeMap<String, ParamValue>,
}

/// A camera move waiting for the bounds it needs (computed on workers).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewRequest {
    /// Home: default orientation, everything in view.
    Reset,
    /// F with nothing targeted: everything in view, same orientation.
    FrameAll,
    /// F: the current target in view, same orientation.
    FrameTarget,
}

/// How long a camera move to an item found from the tree takes (seconds).
pub const FRAME_MOVE_SECONDS: f64 = 0.2;
/// After it, the selection pulses twice over this long (seconds).
pub const PULSE_SECONDS: f64 = 0.6;
/// How many pulses.
const PULSES: f64 = 2.0;
/// After an object's row is clicked, the tree keeps listing the defects it
/// listed for this long (seconds; egui's double-click window is 0.3 s), so
/// the rows don't move under a double-click's second click.
pub const TREE_HOLD_SECONDS: f64 = 0.35;
/// A framing from the tree that is still waiting for what it needs (the
/// item's bounds, the picking BVHs) after this long gives up, saying so,
/// rather than wait (and redraw) forever.
pub const FIND_GIVE_UP_SECONDS: f64 = 10.0;

/// What a row of the tree ("Objects & Defects") stands for. Every row
/// behaves the same way (M4 Task 8a): click selects it, double-click or
/// Enter selects it and frames it, hovering highlights it in the view.
#[derive(Debug, Clone, PartialEq)]
pub enum TreeItem {
    /// An object: selecting it makes it active.
    Object(ObjectId),
    /// The active object as a whole ("Model"): selecting it clears the
    /// selection (the whole object is the target).
    Model(ObjectId),
    /// A defect of the active object.
    Defect(ObjectId, Defect),
}

/// A row double-clicked: frame its item once what that needs is ready (its
/// bounds, the picking BVHs).
#[derive(Debug, Clone)]
struct Find {
    item: TreeItem,
    /// When it was asked for (input time): it gives up after
    /// [`FIND_GIVE_UP_SECONDS`].
    since: f64,
    /// The revision a defect belongs to (a later one may not have it).
    mesh: Option<Mesh>,
}

/// A camera move under way, the camera it last set (a different one
/// means the user moved it: theirs wins), and what pulses after it.
#[derive(Debug, Clone)]
struct Moving {
    path: CameraMove,
    last: OrbitCamera,
    pulse: Option<(ObjectId, OverlayTarget)>,
}

/// One overlay slot: what it was requested for, and the result once built.
struct OverlaySlot {
    object: ObjectId,
    mesh: Mesh,
    target: OverlayTarget,
    /// The local origin of the render mesh it's drawn over (see
    /// `Workers::build_overlay`).
    origin: glam::DVec3,
    ready: Option<Overlay>,
}

/// Which overlays the 3D view shows (the legend's switches; View ▸ Show
/// defects hides every defect overlay at once).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverlaysShown {
    pub selection: bool,
    pub hover: bool,
    pub holes: bool,
    pub cracks: bool,
    /// Loose pieces under remove_small_components' threshold.
    pub loose_pieces: bool,
    pub non_manifold: bool,
    pub recent_repairs: bool,
}

impl Default for OverlaysShown {
    fn default() -> Self {
        OverlaysShown {
            selection: true,
            hover: true,
            holes: true,
            cracks: true,
            loose_pieces: true,
            non_manifold: true,
            recent_repairs: true,
        }
    }
}

/// Where a hover pick is made: the pointer over the 3D view, the camera
/// and the revision it looks at. A change in any of them asks again.
#[derive(Debug, Clone, Copy, PartialEq)]
struct HoverQuery {
    px: (f64, f64),
    size: (f64, f64),
    camera: OrbitCamera,
    revision: RevisionId,
    tangled: bool,
}

/// Hover picking in the 3D view (MF-RENDER-004): at most one pick in
/// flight; the newest pointer position wins, and a result for an older
/// one is dropped.
#[derive(Debug, Default)]
struct HoverPicks {
    /// Where the pointer is now (`None`: not over the view).
    want: Option<HoverQuery>,
    /// The last query sent, and its number.
    sent: Option<HoverQuery>,
    seq: u64,
    in_flight: bool,
}

/// A tangled edge being looked at (edges aren't a selection kind; its next
/// action selects its faces).
#[derive(Debug, Clone)]
pub struct Inspected {
    pub object: ObjectId,
    pub mesh: Mesh,
    pub defect: Defect,
}

pub struct MeshFixerApp {
    pub session: Session,
    pub active: Option<ObjectId>,
    pub camera: Option<OrbitCamera>,
    /// Analyses keyed by revision identity (pointer, so recognizing a
    /// revision is O(1) on the UI thread).
    analyses: AnalysisCache<Analysis>,
    analyzer: LatestWorker<worker::AnalysisRequest>,
    /// Last revision submitted for analysis.
    requested: Option<(Mesh, [f64; 12])>,
    /// The last revision whose analysis failed: not retried (it would fail
    /// again), and not shown as still analysing.
    analysis_failed: Option<(Mesh, [f64; 12])>,
    /// Vertices currently drawn for the active object, tagged with the
    /// exact mesh revision they were built for. May belong to the previous
    /// revision of the active object while the new one is analyzed (the
    /// viewport's `updating` flag says so); switching to a *different*
    /// active object before its own analysis returns must not keep
    /// showing this, or the old object's geometry would be drawn at the
    /// new object's transform in full colour, and the old object would
    /// vanish instead (MF-ISSUE-042 review I4).
    /// Which object and revision they are; while the active object's new
    /// revision is analysed, its previous revision stays on screen.
    displayed: Option<(ObjectId, Mesh, Arc<RenderMesh>)>,
    /// Render-only vertices for every object that isn't active
    /// (MF-ISSUE-042): the viewport still draws them, dimmed, at their
    /// transform. Keyed by object id; the `Mesh` says which revision the
    /// vertices belong to, so a stale build for a since-changed object is
    /// never shown.
    object_vertices: BTreeMap<ObjectId, (Mesh, Arc<RenderMesh>)>,
    /// Object ids with a build in flight, and the mesh it was requested for
    /// (so a second change before the first build returns still gets
    /// re-requested).
    requested_vertices: BTreeMap<ObjectId, Mesh>,
    /// Label of the running import/operation/export. Blocks new commands.
    pub busy: Option<String>,
    /// Progress and cancellation of the job `busy` names.
    job: Option<RunningJob>,
    /// The last 1000 frames' CPU, GPU and interval times (MF-RT-001).
    pub frame_times: FrameTimes,
    /// The whole-object tool open in Properties (an operation id).
    pub tool: Option<String>,
    pub op_params: BTreeMap<String, BTreeMap<String, ParamValue>>,
    pub show_boundaries: bool,
    pub error: Option<String>,
    pub status: Option<String>,
    /// Every warning from the latest import (MF-ISSUE-004), not just the last.
    pub warnings: Vec<String>,
    /// Lines of the last replay report, shown in a window.
    pub replay_report: Option<Vec<String>>,
    /// What the next operation targets, bound to the current revision. It is
    /// translated after every history move (MF-SEL-003).
    pub selection: Option<Selection>,
    /// A tangled edge shown in Properties (only while nothing is selected).
    pub inspected: Option<Inspected>,
    /// What the pointer is over in the tree this frame (outlined in the view).
    pub hover: Option<(ObjectId, OverlayTarget)>,
    /// What the pointer is over in the 3D view (a defect of the active
    /// object), from the latest hover pick.
    view_hover: Option<ViewHover>,
    hover_picks: HoverPicks,
    /// Each object's picking BVH, for its current revision only.
    bvhs: BTreeMap<ObjectId, (Mesh, Arc<Bvh>)>,
    /// Builds BVHs on the background pool, a newer request (a revision
    /// changed) cancelling the one running.
    bvh_builder: LatestWorker<Vec<(ObjectId, Mesh)>>,
    /// The revisions last asked for.
    bvh_requested: Vec<(ObjectId, Mesh)>,
    /// Revisions whose topology was asked for on its own: they have their
    /// BVH, but their topology isn't cached (whoever was computing it was
    /// cancelled, or it was released). Picking waits for it.
    topology_requested: Vec<Mesh>,
    overlays: BTreeMap<OverlayPurpose, OverlaySlot>,
    /// Which overlays are switched on (the legend; View menu).
    pub shown: OverlaysShown,
    /// Faces made by the active object's last few commands, for the
    /// revision they were found for.
    recent: Option<(ObjectId, Mesh, Arc<[u32]>)>,
    /// The last face-state request's number: a later one wins.
    state_seq: u64,
    /// Help > Controls window.
    pub show_controls: bool,
    view_request: Option<ViewRequest>,
    /// A tree row to frame (double-click or Enter).
    find: Option<Find>,
    /// The camera move framing it.
    moving: Option<Moving>,
    /// The selection's pulse after a framing: when it started, and for
    /// which target (another target doesn't pulse).
    pulse: Option<(f64, (ObjectId, OverlayTarget))>,
    /// The selection's emphasis this frame (0–1: the pulse).
    emphasis: f32,
    /// This frame's input time (seconds).
    now: f64,
    /// The object the tree keeps expanded until a time (see
    /// [`TREE_HOLD_SECONDS`]).
    tree_hold: Option<(Option<ObjectId>, f64)>,
    pub viewport: Viewport,
    workers: Workers,
    /// Shares the workers' upload totals.
    gpu_totals: worker::Gpu,
    rx: Receiver<Message>,
}

impl MeshFixerApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        initial: Option<PathBuf>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Self::with_settings(cc, initial, Settings::default())
    }

    pub fn with_settings(
        cc: &eframe::CreationContext<'_>,
        initial: Option<PathBuf>,
        settings: Settings,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .ok_or("MeshFixer needs the wgpu renderer, but it is unavailable")?;

        let (tx, rx) = channel::<Message>();
        theme::apply(&cc.egui_ctx);
        // Cmd/Ctrl + / − / 0 belong to the 3D view, not to scaling the UI.
        cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
        let gpu = worker::Gpu::new(render_state.device.clone(), render_state.queue.clone());
        let workers = Workers::new(
            tx.clone(),
            cc.egui_ctx.clone(),
            settings.threads,
            gpu.sibling(),
        );
        let gpu_totals = gpu.clone();
        let analyze = settings.analyze.unwrap_or(worker::analyze);
        let bvh_builder = {
            let (tx, ctx) = (tx.clone(), cc.egui_ctx.clone());
            LatestWorker::new(
                workers.background(),
                move |revisions: Vec<(ObjectId, Mesh)>, run: &RunContext| {
                    // Each is reported as soon as it's built: a superseded
                    // request keeps the ones it finished.
                    for (object, mesh) in revisions {
                        let bvh = Arc::new(Bvh::build(&mesh, run)?);
                        // Picking needs the topology too (hole edges,
                        // pieces): built here, so a hover never builds it
                        // on the picking lane. If the analysis is computing
                        // it too, the first to finish publishes it (no
                        // waiting: see `Mesh::topology_with`).
                        mesh.topology_with(run)?;
                        // allow-swallow: the receiver only disappears while the app is closing
                        let _ = tx.send(Message::BvhBuilt { object, mesh, bvh });
                        ctx.request_repaint();
                    }
                    Ok(())
                },
                |()| {},
            )
        };
        let mut app = MeshFixerApp {
            session: Session::new(),
            active: None,
            camera: None,
            analyses: AnalysisCache::new(4),
            analyzer: {
                let ctx = cc.egui_ctx.clone();
                LatestWorker::new(
                    workers.background(),
                    move |request: worker::AnalysisRequest, run: &RunContext| {
                        // A panic is reported for that revision, not left
                        // to abort the app (or to look like work forever).
                        let (mesh, transform) = (request.mesh.clone(), request.transform);
                        let analysed =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                analyze(request, run, &gpu)
                            }));
                        match analysed {
                            Ok(result) => result,
                            Err(payload) => {
                                let Message::Failed { message, .. } =
                                    worker::failed("analysis", false, payload)
                                else {
                                    unreachable!("worker::failed makes a Failed message")
                                };
                                Ok(Message::AnalysisFailed {
                                    mesh,
                                    transform,
                                    message,
                                })
                            }
                        }
                    },
                    move |msg| {
                        // allow-swallow: the receiver only disappears while the app is closing
                        let _ = tx.send(msg);
                        ctx.request_repaint();
                    },
                )
            },
            requested: None,
            analysis_failed: None,
            displayed: None,
            object_vertices: BTreeMap::new(),
            requested_vertices: BTreeMap::new(),
            busy: None,
            job: None,
            frame_times: FrameTimes::new(),
            tool: None,
            op_params: BTreeMap::new(),
            show_boundaries: true,
            error: None,
            status: None,
            warnings: Vec::new(),
            replay_report: None,
            selection: None,
            inspected: None,
            hover: None,
            view_hover: None,
            hover_picks: HoverPicks::default(),
            bvhs: BTreeMap::new(),
            bvh_builder,
            bvh_requested: Vec::new(),
            topology_requested: Vec::new(),
            overlays: BTreeMap::new(),
            shown: OverlaysShown::default(),
            recent: None,
            state_seq: 0,
            show_controls: false,
            view_request: None,
            find: None,
            moving: None,
            pulse: None,
            emphasis: 0.0,
            now: 0.0,
            tree_hold: None,
            viewport: Viewport::new(render_state.clone(), theme::palette()),
            workers,
            gpu_totals,
            rx,
        };
        if let Some(path) = initial {
            app.open(path);
        }
        Ok(app)
    }

    pub fn active_mesh(&self) -> Option<&Mesh> {
        self.active
            .and_then(|id| self.session.document().object(id))
            .and_then(|o| o.mesh())
    }

    pub fn open(&mut self, path: PathBuf) {
        let run = self.start_job(format!("Importing {}", path.display()), true);
        self.workers.import(path, run);
    }

    pub fn open_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Meshes", &["stl", "obj", "3mf"])
            .pick_file()
        {
            self.open(path);
        }
    }

    /// Add an imported model to the document (what finishing an import
    /// does): its objects become part of the history, the first is made
    /// active and the camera is refitted.
    pub fn import_model(&mut self, model: ImportedModel) {
        let (ids, warnings) = self.session.import_model(model);
        self.warnings = warnings;
        if !self.warnings.is_empty() {
            self.status = Some(format!("{} import warning(s)", self.warnings.len()));
        }
        match ids.first() {
            Some(&id) => {
                self.set_active(id);
                // Refit even into a non-empty document (MF-ISSUE-042
                // review I5): otherwise the camera keeps framing the old
                // document and the newly imported object can be off screen.
                self.fit_camera();
            }
            None => self.error = Some("The file contains no mesh objects".into()),
        }
    }

    /// Mark a job as running (`label` in the status bar) and return the
    /// context it runs with. `cancellable` jobs get a Cancel button.
    fn start_job(&mut self, label: String, cancellable: bool) -> RunContext {
        let run = RunContext::none();
        self.busy = Some(label);
        self.job = Some(RunningJob {
            run: cancellable.then(|| run.clone()),
            cancel_requested: false,
        });
        run
    }

    /// The running job is over: returns whether its cancel was requested.
    fn finish_job(&mut self) -> bool {
        self.busy = None;
        self.job.take().is_some_and(|j| j.cancel_requested)
    }

    /// The running job's stage and fraction (0–1), if it reports progress.
    pub fn job_progress(&self) -> Option<(&'static str, f64)> {
        self.job
            .as_ref()?
            .run
            .as_ref()
            .map(|r| r.progress.snapshot())
    }

    /// True while a job runs that Cancel (or Esc) would stop.
    pub fn can_cancel(&self) -> bool {
        self.job
            .as_ref()
            .is_some_and(|j| j.run.is_some() && !j.cancel_requested)
    }

    /// True once Cancel was pressed and the job hasn't stopped yet.
    pub fn is_cancelling(&self) -> bool {
        self.job.as_ref().is_some_and(|j| j.cancel_requested)
    }

    /// Ask the running job to stop (MF-RT-002). It stops within 250 ms and
    /// leaves the document unchanged; the status then says so.
    pub fn cancel_job(&mut self) {
        if let Some(job) = self.job.as_mut()
            && let Some(run) = &job.run
        {
            run.cancel.cancel();
            job.cancel_requested = true;
        }
    }

    /// How the job pool was sized (shown under Help).
    pub fn pool_sizes(&self) -> PoolSizes {
        self.workers.sizes
    }

    /// Write the command log for the current revision path.
    pub fn save_log_to(&mut self, dir: &std::path::Path) -> Result<(), String> {
        self.session.save_log(dir).map_err(|e| e.to_string())
    }

    pub fn save_log_dialog(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_file_name("session.mfx.log")
            .save_file()
        else {
            return;
        };
        match self.save_log_to(&path) {
            Ok(()) => self.status = Some(format!("Saved command log {}", path.display())),
            Err(e) => self.error = Some(format!("Saving command log failed: {e}")),
        }
    }

    pub fn replay_dialog(&mut self) {
        if let Some(dir) = rfd::FileDialog::new().pick_folder() {
            // Replay can't be cancelled yet: it has no Cancel button.
            self.start_job(format!("Replaying {}", dir.display()), false);
            self.workers.replay(dir);
        }
    }

    /// Jump to any revision in the DAG, including an abandoned branch.
    pub fn checkout(&mut self, id: RevisionId) {
        if self.busy.is_none() && self.session.checkout(id) {
            self.after_history_move();
        }
    }

    pub fn export_dialog(&mut self) {
        let Some(mesh) = self.active_mesh().cloned() else {
            return;
        };
        let Some(path) = rfd::FileDialog::new()
            .add_filter("3MF (all objects)", &["3mf"])
            .add_filter("Binary STL", &["stl"])
            .add_filter("OBJ", &["obj"])
            .save_file()
        else {
            return;
        };
        match Format::from_path(&path) {
            Some(Format::ThreeMf) => match self.document_model() {
                Ok(model) => {
                    let run = self.start_job(format!("Exporting {}", path.display()), true);
                    self.workers
                        .export(worker::ExportJob::Model(model), path, run);
                }
                Err(e) => self.error = Some(e),
            },
            Some(format) => {
                let run = self.start_job(format!("Exporting {}", path.display()), true);
                self.workers
                    .export(worker::ExportJob::Mesh(mesh, format), path, run);
            }
            None => self.error = Some(format!("Unsupported export format: {}", path.display())),
        }
    }

    /// The whole document as a 3MF model (every object, transforms, units).
    pub fn document_model(&self) -> Result<mf_io::threemf::Model, String> {
        let doc = self.session.document();
        let objects = doc
            .objects()
            .iter()
            .map(|o| {
                let mesh = o
                    .mesh()
                    .ok_or_else(|| format!("object '{}' is not a mesh", o.name))?;
                Ok(mf_io::threemf::ModelObject {
                    name: o.name.clone(),
                    mesh: mesh.clone(),
                    transform: o.transform,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(mf_io::threemf::Model {
            unit: doc.units(),
            objects,
        })
    }

    /// Current parameter values for an operation, initialized from spec defaults.
    pub fn params_for(&mut self, op_id: &str) -> &mut BTreeMap<String, ParamValue> {
        self.op_params.entry(op_id.to_owned()).or_insert_with(|| {
            mf_spec::builtin::get(op_id)
                .map(|s| {
                    s.parameters
                        .iter()
                        .map(|p| (p.name.clone(), p.kind.default_value()))
                        .collect()
                })
                // allow-swallow: an operation id with no spec has no parameters (empty map)
                .unwrap_or_default()
        })
    }

    /// The operations Properties offers for the current target: the ones
    /// valid for the selected hole(s) or piece(s), or, when nothing is
    /// targeted (or the target has none), the whole-object operations.
    pub fn tools(&self) -> Vec<Tool> {
        let targeted = self.targeted_tools();
        if !targeted.is_empty() || self.target_blocks_whole_object() {
            return targeted;
        }
        mf_spec::builtin::specs()
            .iter()
            .filter(|s| !s.is_read_only() && s.target.whole_object)
            .map(|s| Tool {
                op_id: s.id.clone(),
                // allow-swallow: an operation without plain words is named by its id
                label: operation_label(&s.id, None).unwrap_or_else(|| s.id.clone()),
                selection: None,
                fixed: BTreeMap::new(),
            })
            .collect()
    }

    /// True when the target is a defect with no operation yet (a crack, a
    /// hole over the fill limit): Properties says what it needs instead of
    /// offering whole-object tools that wouldn't fix it.
    fn target_blocks_whole_object(&self) -> bool {
        let Some((sel, analysis)) = self.current_selection().zip(self.current_analysis()) else {
            return false;
        };
        sel.kind == SelectionKind::BoundaryLoop
            && sel.ids.iter().any(|&id| {
                !matches!(
                    analysis.defects.find(sel.kind, id),
                    Some(Defect::Hole { .. })
                )
            })
    }

    /// The selection of the active object at the current revision.
    pub fn current_selection(&self) -> Option<&Selection> {
        self.selection
            .as_ref()
            .filter(|s| Some(s.object) == self.active && s.revision == self.session.current())
    }

    fn targeted_tools(&self) -> Vec<Tool> {
        let (Some(sel), Some(analysis)) = (self.current_selection(), self.current_analysis())
        else {
            return Vec::new();
        };
        let defects: Option<Vec<&Defect>> = sel
            .ids
            .iter()
            .map(|&id| analysis.defects.find(sel.kind, id))
            .collect();
        let Some(defects) = defects else {
            return Vec::new();
        };
        let n = defects.len();
        let tool = |op_id: &str, fixed: BTreeMap<String, ParamValue>| Tool {
            op_id: op_id.to_owned(),
            label: operation_label(op_id, Some((sel.kind, n)))
                .expect("built-in operations have plain names"),
            selection: Some(sel.clone()),
            fixed,
        };
        let holes: Option<Vec<usize>> = defects
            .iter()
            .map(|d| match d {
                Defect::Hole { edges, .. } => Some(*edges),
                _ => None,
            })
            .collect();
        if let Some(edges) = holes {
            let default = spec_default("mesh.fill_holes", "max_loop_edges");
            let largest = edges.into_iter().max().map_or(0, |e| e as i64);
            return vec![tool(
                "mesh.fill_holes",
                BTreeMap::from([(
                    "max_loop_edges".into(),
                    ParamValue::Int(largest.max(default)),
                )]),
            )];
        }
        let pieces: Option<Vec<usize>> = defects
            .iter()
            .map(|d| match d {
                Defect::LoosePiece { triangles, .. } => Some(*triangles),
                _ => None,
            })
            .collect();
        if let Some(triangles) = pieces {
            let largest = triangles.into_iter().max().map_or(0, |t| t as i64);
            return vec![tool(
                "mesh.remove_small_components",
                BTreeMap::from([
                    ("max_triangles".into(), ParamValue::Int(largest + 1)),
                    ("keep_largest".into(), ParamValue::Bool(true)),
                ]),
            )];
        }
        Vec::new()
    }

    /// Validate on the UI thread, run on a worker, commit when it returns.
    pub fn apply(&mut self, tool: &Tool) {
        let Some(target) = self.active else { return };
        let mut params = self.params_for(&tool.op_id).clone();
        params.extend(tool.fixed.clone());
        let raw_params = params
            .iter()
            .map(|(k, v)| (k.clone(), v.to_string()))
            .collect();
        match self.session.prepare(Invocation {
            selection: tool.selection.clone(),
            op_id: tool.op_id.clone(),
            target,
            raw_params,
        }) {
            Ok(prepared) => {
                let run = self.start_job(format!("{}…", tool.label), true);
                self.workers.run(prepared, run);
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    /// The history breadcrumb: the current path from the first import, then
    /// the steps redo would take (greyed). Root ("New document") is left out.
    pub fn breadcrumb(&self) -> Vec<(RevisionId, String, bool)> {
        let current = self.session.current();
        let mut past = true;
        self.session
            .linear_view()
            .into_iter()
            .filter_map(|id| {
                let node = self.session.node(id)?;
                let reached = past;
                if id == current {
                    past = false;
                }
                node.parent?;
                Some((id, step_label(node), reached))
            })
            .collect()
    }

    pub fn undo(&mut self) {
        if self.busy.is_none() {
            self.session.undo();
            self.after_history_move();
        }
    }

    pub fn redo(&mut self) {
        if self.busy.is_none() {
            self.session.redo();
            self.after_history_move();
        }
    }

    fn after_history_move(&mut self) {
        self.sync_active();
        self.retarget_selection();
    }

    /// Translate the selection to the current revision (MF-SEL-003). A loop
    /// that was filled clears it with a message; anything not mappable clears
    /// it with the reason. Never applied to a revision it wasn't translated to.
    fn retarget_selection(&mut self) {
        let Some(sel) = self.selection.take() else {
            return;
        };
        let current = self.session.current();
        if sel.revision == current {
            self.selection = Some(sel);
            return;
        }
        match self.session.translate(&sel, current) {
            Translation::Mapped(m) => self.selection = Some(m),
            Translation::Partial { mapped, lost } => {
                self.status = Some(format!("Selection partly lost ({} element(s))", lost.len()));
                self.selection = Some(mapped);
            }
            Translation::Failed(TranslateError::LoopClosed { faces }) => {
                let plural = if faces.len() == 1 { "" } else { "s" };
                self.status = Some(format!(
                    "Hole filled ({} face{plural}); selection cleared",
                    faces.len()
                ));
            }
            Translation::Failed(e) => self.status = Some(selection_lost(&e)),
            Translation::Ambiguous { candidates } => {
                self.status = Some(format!(
                    "Selection cleared: it could now be any of {} parts",
                    candidates.len()
                ))
            }
        }
    }

    /// Select what's under a click: a boundary loop within 8 px of any
    /// object (when its topology is ready), or the faces of a drawn
    /// non-manifold edge within 8 px, otherwise the nearest clicked face's
    /// component across every object (ray cast on a worker). A hit
    /// makes its object active (MF-ISSUE-042); shift adds to a same-kind,
    /// same-object selection.
    pub fn click(&mut self, click: Click) {
        let (Some(active), Some(cam)) = (self.active, self.camera) else {
            return;
        };
        let targets = self.pick_targets();
        if targets.is_empty() {
            return;
        }
        self.workers.pick(
            targets,
            active,
            cam,
            cam.target,
            click.size,
            click.px,
            click.shift,
        );
    }

    /// Every object a click or hover can land on, with its BVH if it's
    /// built (see [`PickTarget::bvh`] for a click before then) and, for the
    /// active object once analysed, its defects. O(objects).
    fn pick_targets(&self) -> Vec<PickTarget> {
        let analysis = self.current_analysis();
        self.session
            .document()
            .objects()
            .iter()
            .filter_map(|o| {
                let mesh = o.mesh()?;
                let active = Some(o.id) == self.active;
                Some(PickTarget {
                    object: o.id,
                    mesh: mesh.clone(),
                    model: model_matrix(&o.transform),
                    bvh: self
                        .bvhs
                        .get(&o.id)
                        .filter(|(m, _)| Mesh::ptr_eq(m, mesh))
                        .map(|(_, b)| b.clone()),
                    defects: analysis.filter(|_| active).map(|a| a.defects.clone()),
                    // Only the active object's tangled edges are drawn.
                    tangled: active && self.defect_shown(self.shown.non_manifold),
                })
            })
            .collect()
    }

    /// True once every object's current revision has its picking BVH and
    /// topology: hover picks wait for them (MF-RENDER-004). The BVH is
    /// checked first (O(1), and the builder computes the topology before
    /// reporting one); the topology check never waits for a computation
    /// holding its lock (fix round 2, N1).
    pub fn picking_ready(&self) -> bool {
        self.session.document().objects().iter().all(|o| {
            o.mesh().is_none_or(|mesh| {
                self.bvhs
                    .get(&o.id)
                    .is_some_and(|(m, _)| Mesh::ptr_eq(m, mesh))
                    && mesh.has_cached_topology()
            })
        })
    }

    /// True once object `id`'s current revision has its picking BVH (tests
    /// and diagnostics).
    pub fn has_picking_bvh(&self, id: ObjectId) -> bool {
        let Some(mesh) = self.session.document().object(id).and_then(|o| o.mesh()) else {
            return false;
        };
        self.bvhs
            .get(&id)
            .is_some_and(|(m, _)| Mesh::ptr_eq(m, mesh))
    }

    /// Ask for the BVH of every object revision that lacks one (the active
    /// object first), and let go of those for revisions no longer current.
    /// O(objects) on the UI thread; the builds run on the background pool.
    fn refresh_bvhs(&mut self) {
        let doc = self.session.document();
        let stale: Vec<ObjectId> = self
            .bvhs
            .iter()
            .filter(|(id, (m, _))| {
                doc.object(**id)
                    .and_then(|o| o.mesh())
                    .is_none_or(|current| !Mesh::ptr_eq(current, m))
            })
            .map(|(id, _)| *id)
            .collect();
        for id in stale {
            if let Some(old) = self.bvhs.remove(&id) {
                self.workers.discard(old);
            }
        }
        let mut missing: Vec<(ObjectId, Mesh)> = doc
            .objects()
            .iter()
            .filter_map(|o| Some((o.id, o.mesh()?.clone())))
            .filter(|(id, _)| !self.bvhs.contains_key(id))
            .collect();
        missing.sort_by_key(|(id, _)| Some(*id) != self.active);
        let new = missing.iter().any(|(id, m)| {
            !self
                .bvh_requested
                .iter()
                .any(|(r, rm)| r == id && Mesh::ptr_eq(rm, m))
        });
        // What was asked for holds revisions alive: let go of them once
        // they're built or replaced, on the pool (the last handle on a big
        // revision frees it).
        if new {
            let old = std::mem::replace(&mut self.bvh_requested, missing.clone());
            self.workers.discard(old);
            self.bvh_builder.submit(missing);
        } else if missing.is_empty() && !self.bvh_requested.is_empty() {
            self.workers
                .discard(std::mem::take(&mut self.bvh_requested));
        }
        self.refresh_topologies();
    }

    /// Picking needs every object's topology as well as its BVH. The BVH
    /// builder computes it, but it can still be missing with the BVH built
    /// (whoever was computing it was cancelled, or it was released): then
    /// it's computed on its own, once per revision. O(objects).
    fn refresh_topologies(&mut self) {
        let doc = self.session.document();
        let (done, keep): (Vec<Mesh>, Vec<Mesh>) = std::mem::take(&mut self.topology_requested)
            .into_iter()
            .partition(|m| {
                m.has_cached_topology()
                    || !doc
                        .objects()
                        .iter()
                        .any(|o| o.mesh().is_some_and(|c| Mesh::ptr_eq(c, m)))
            });
        self.topology_requested = keep;
        if !done.is_empty() {
            self.workers.discard(done);
        }
        let missing: Vec<Mesh> = self
            .bvhs
            .iter()
            .filter(|(id, (m, _))| {
                doc.object(**id)
                    .and_then(|o| o.mesh())
                    .is_some_and(|c| Mesh::ptr_eq(c, m))
                    && !m.has_cached_topology()
                    && !self.topology_requested.iter().any(|r| Mesh::ptr_eq(r, m))
            })
            .map(|(_, (m, _))| m.clone())
            .collect();
        for mesh in missing {
            self.workers.warm_topology(mesh.clone());
            self.topology_requested.push(mesh);
        }
    }

    /// The view's hover (see [`HoverPicks`]): `pointer` is where it rests
    /// over the 3D view, if it does. A pick is sent when the pointer, the
    /// camera or the revision changed and none is in flight, once every
    /// object has its BVH and the active one its analysis; until then the
    /// hover just doesn't update.
    fn update_hover(&mut self, pointer: Option<((f64, f64), (f64, f64))>) {
        let want = match (pointer, self.camera, self.active) {
            (Some((px, size)), Some(camera), Some(_)) => Some(HoverQuery {
                px,
                size,
                camera,
                revision: self.session.current(),
                tangled: self.defect_shown(self.shown.non_manifold),
            }),
            _ => None,
        };
        let picks = &mut self.hover_picks;
        if want.is_none() {
            self.view_hover = None;
            // Coming back to the same spot asks again (fix round 1, P3).
            picks.sent = None;
        }
        picks.want = want;
        let Some(query) = want else { return };
        if picks.in_flight
            || picks.sent == want
            || !self.picking_ready()
            || self.current_analysis().is_none()
        {
            return;
        }
        let targets = self.pick_targets();
        let picks = &mut self.hover_picks;
        picks.seq += 1;
        picks.sent = want;
        picks.in_flight = true;
        let seq = picks.seq;
        self.workers.hover(
            targets,
            query.camera,
            query.camera.target,
            query.size,
            query.px,
            seq,
        );
    }

    /// What the pointer is over in the 3D view, if it's on the active
    /// object's current revision.
    pub fn view_hover(&self) -> Option<&ViewHover> {
        self.view_hover.as_ref().filter(|h| {
            Some(h.object) == self.active
                && self
                    .session
                    .document()
                    .object(h.object)
                    .and_then(|o| o.mesh())
                    .is_some_and(|m| Mesh::ptr_eq(m, &h.mesh))
        })
    }

    /// Where the pointer rests over the 3D view (window points), if it does.
    pub fn view_pointer(&self) -> Option<egui::Pos2> {
        let q = self.hover_picks.want?;
        let rect = self.viewport.last_rect?;
        Some(rect.left_top() + egui::vec2(q.px.0 as f32, q.px.1 as f32))
    }

    /// Make `defect` (of the active object) the current target, as a tree
    /// click does.
    pub fn select_defect(&mut self, defect: &Defect) {
        let Some(object) = self.active else { return };
        match defect.selection() {
            Some((kind, id)) => self.select(object, kind, vec![id], false),
            None => {
                let Some(mesh) = self.active_mesh().cloned() else {
                    return;
                };
                self.selection = None;
                self.status = None;
                self.inspected = Some(Inspected {
                    object,
                    mesh,
                    defect: defect.clone(),
                });
            }
        }
    }

    /// Select the faces of an inspected tangled edge (its next action).
    pub fn select_tangled_faces(&mut self) {
        let Some(Inspected {
            object,
            defect: Defect::Tangled { faces, .. },
            ..
        }) = self.inspected.take()
        else {
            return;
        };
        self.status = None;
        self.selection = Some(Selection {
            object,
            revision: self.session.current(),
            kind: SelectionKind::Faces,
            ids: faces,
        });
    }

    /// The inspected tangled edge, if it's still on the revision shown.
    pub fn current_inspected(&self) -> Option<&Inspected> {
        self.inspected.as_ref().filter(|i| {
            self.selection.is_none()
                && Some(i.object) == self.active
                && self.active_mesh().is_some_and(|m| Mesh::ptr_eq(m, &i.mesh))
        })
    }

    fn select(&mut self, object: ObjectId, kind: SelectionKind, ids: Vec<u32>, add: bool) {
        self.inspected = None;
        // A status about an earlier target would contradict the new one.
        self.status = None;
        let revision = self.session.current();
        match self.selection.as_mut() {
            Some(sel)
                if add && sel.kind == kind && sel.object == object && sel.revision == revision =>
            {
                sel.ids.extend(ids);
                sel.ids.sort_unstable();
                sel.ids.dedup();
            }
            _ => {
                let mut ids = ids;
                ids.sort_unstable();
                ids.dedup();
                self.selection = Some(Selection {
                    object,
                    revision,
                    kind,
                    ids,
                })
            }
        }
    }

    pub fn clear_selection(&mut self) {
        if self.selection.is_some() || self.inspected.is_some() {
            self.status = None;
        }
        self.selection = None;
        self.inspected = None;
    }

    /// Where a world point appears in the window (for overlays and tests).
    pub fn screen_point(&self, world: [f64; 3]) -> Option<egui::Pos2> {
        let (rect, cam) = (self.viewport.last_rect?, self.camera?);
        let size = (rect.width() as f64, rect.height() as f64);
        let (x, y) = screen_position(&cam, cam.target, size, world)?;
        Some(egui::pos2(rect.left() + x as f32, rect.top() + y as f32))
    }

    /// Make `id` the active object. A selection belongs to one object, so
    /// switching to another clears it (its IDs mean nothing there). The
    /// camera is shared by the whole document (MF-ISSUE-042) and keeps
    /// framing every object, so activating another one doesn't move it.
    pub fn set_active(&mut self, id: ObjectId) {
        if self.active != Some(id) && self.selection.as_ref().is_some_and(|s| s.object != id) {
            self.selection = None;
            self.status = Some("Selection cleared (another object is active)".into());
        }
        if self.active != Some(id) {
            self.inspected = None;
        }
        self.active = Some(id);
    }

    /// Loop IDs the viewport highlights: only a loop selection of the active
    /// object at the current revision.
    pub fn highlighted_loops(&self) -> Option<Vec<u32>> {
        let sel = self.selection.as_ref().filter(|s| {
            s.kind == SelectionKind::BoundaryLoop
                && Some(s.object) == self.active
                && s.revision == self.session.current()
        })?;
        Some(sel.ids.clone())
    }

    /// Keep `active` pointing at an object that exists in the current document.
    fn sync_active(&mut self) {
        let doc = self.session.document();
        if self.active.and_then(|id| doc.object(id)).is_none() {
            self.active = doc.objects().last().map(|o| o.id);
        }
    }

    fn handle(&mut self, msg: Message) {
        match msg {
            Message::Imported(result) => {
                let too_late = self.finish_job();
                match result {
                    Ok(model) => {
                        self.import_model(model);
                        if too_late {
                            self.status = Some(TOO_LATE.into());
                        }
                    }
                    Err(IoError::Cancelled) => self.status = Some(CANCELLED.into()),
                    Err(e) => self.error = Some(format!("Import failed: {e}")),
                }
            }
            Message::Completed(result) => {
                // Once the run returned Ok, cancelling no longer applies:
                // the result is committed (one outcome; MF-CMD-006).
                let too_late = self.finish_job();
                let op = result.as_ref().as_ref().map_or("", |c| c.op_id);
                let report = |r: &serde_json::Value| {
                    let r = plain_report(op, r);
                    if too_late {
                        format!("{TOO_LATE}: {r}")
                    } else {
                        r
                    }
                };
                match (*result).and_then(|c| self.session.commit(c)) {
                    Ok(CommitOutcome::NewRevision { report: r }) => {
                        self.status = Some(report(&r));
                        self.retarget_selection();
                    }
                    Ok(CommitOutcome::Report(r)) => {
                        self.status = Some(report(&r));
                    }
                    Ok(CommitOutcome::NoChange { report: r }) => {
                        self.status = Some(format!("Nothing to change: {}", report(&r)));
                    }
                    Err(CommandError::Cancelled) => self.status = Some(CANCELLED.into()),
                    Err(e) => self.error = Some(e.to_string()),
                }
            }
            Message::Hovered {
                seq,
                hover,
                incomplete,
            } => {
                let picks = &mut self.hover_picks;
                if seq == picks.seq {
                    picks.in_flight = false;
                    if incomplete {
                        // Ask again for the same spot once picking is
                        // ready (fix round 2, N3).
                        picks.sent = None;
                    }
                    // A result for where the pointer was is stale once it
                    // has moved on: the next frame asks for where it is.
                    if picks.want == picks.sent {
                        self.view_hover = hover;
                    }
                }
            }
            Message::BvhBuilt { object, mesh, bvh } => {
                let current = self
                    .session
                    .document()
                    .object(object)
                    .and_then(|o| o.mesh())
                    .is_some_and(|m| Mesh::ptr_eq(m, &mesh));
                if current {
                    if let Some(old) = self.bvhs.insert(object, (mesh, bvh)) {
                        self.workers.discard(old);
                    }
                } else {
                    self.workers.discard((mesh, bvh));
                }
            }
            Message::Failed { what, job, message } => {
                if job {
                    self.finish_job();
                }
                if what == "hover" {
                    // Its answer will never come: let the next one go, and
                    // ask again for the same spot.
                    self.hover_picks.in_flight = false;
                    self.hover_picks.sent = None;
                }
                self.error = Some(format!("Internal error in {what}: {message}"));
            }
            Message::Picked {
                object,
                mesh,
                picked,
                add,
            } => {
                // Ignore a pick for a revision that's no longer on screen,
                // and a miss reported against no mesh at all (the clicked
                // active object had none to begin with).
                let current = self
                    .session
                    .document()
                    .object(object)
                    .and_then(|o| o.mesh());
                let is_current = match (&mesh, current) {
                    (Some(m), Some(current)) => Mesh::ptr_eq(current, m),
                    _ => false,
                };
                if is_current {
                    match picked {
                        Some((kind, ids)) => {
                            // A hit on another object makes it active
                            // (MF-ISSUE-042); `set_active` clears a
                            // selection that belongs to a different object.
                            self.set_active(object);
                            self.select(object, kind, ids, add);
                        }
                        None if !add => self.selection = None,
                        None => {}
                    }
                }
            }
            // The view notices the new states by their generation.
            Message::StatesWritten | Message::TopologyCached => {}
            Message::Overlay {
                purpose,
                object,
                mesh,
                target,
                overlay,
            } => {
                if let Some(slot) = self.overlays.get_mut(&purpose)
                    && slot.object == object
                    && Mesh::ptr_eq(&slot.mesh, &mesh)
                    && slot.target == target
                    && slot.origin == overlay.origin
                {
                    slot.ready = Some(overlay);
                }
            }
            Message::ObjectVertices {
                object,
                mesh,
                render,
            } => {
                if self
                    .requested_vertices
                    .get(&object)
                    .is_some_and(|m| Mesh::ptr_eq(m, &mesh))
                {
                    self.requested_vertices.remove(&object);
                }
                if let Some(old) = self.object_vertices.insert(object, (mesh, render)) {
                    self.workers.discard(old);
                }
            }
            Message::AnalysisFailed {
                mesh,
                transform,
                message,
            } => {
                if self
                    .requested
                    .as_ref()
                    .is_some_and(|(m, t)| Mesh::ptr_eq(m, &mesh) && *t == transform)
                {
                    self.requested = None;
                }
                self.error = Some(format!(
                    "Couldn't analyse this object, so its defects can't be shown \
                     (internal error: {message})"
                ));
                self.analysis_failed = Some((mesh, transform));
            }
            Message::Analyzed { mesh, analysis } => {
                if self
                    .requested
                    .as_ref()
                    .is_some_and(|(m, t)| Mesh::ptr_eq(m, &mesh) && *t == analysis.transform)
                {
                    self.requested = None;
                }
                if let Some(evicted) = self.analyses.insert(mesh, *analysis) {
                    self.workers.discard(evicted);
                }
            }
            Message::Replayed(result) => {
                self.finish_job();
                match result {
                    Ok(lines) => self.replay_report = Some(lines),
                    Err(e) => self.error = Some(format!("Replay failed: {e}")),
                }
            }
            Message::Exported(result) => {
                let too_late = self.finish_job();
                match result {
                    Ok(path) if too_late => {
                        self.status = Some(format!("{TOO_LATE}: exported {}", path.display()))
                    }
                    Ok(path) => self.status = Some(format!("Exported {}", path.display())),
                    Err(IoError::Cancelled) => self.status = Some(CANCELLED_EXPORT.into()),
                    Err(e) => self.error = Some(format!("Export failed: {e}")),
                }
            }
        }
    }

    /// Analysis for exactly the active revision, if it's ready.
    pub fn current_analysis(&self) -> Option<&Analysis> {
        let object = self.session.document().object(self.active?)?;
        self.analysis_for(object.mesh()?, &object.transform)
    }

    /// The analysis of `mesh` measured at `transform`, if it's ready.
    fn analysis_for(&self, mesh: &Mesh, transform: &[f64; 12]) -> Option<&Analysis> {
        self.analyses
            .get(mesh)
            .filter(|a| a.transform == *transform)
    }

    /// Request analysis when the active revision changes.
    fn refresh_analysis(&mut self) {
        let active = self
            .active
            .and_then(|id| self.session.document().object(id));
        let Some((mesh, transform)) = active.and_then(|o| Some((o.mesh()?.clone(), o.transform)))
        else {
            self.displayed = None;
            return;
        };
        if self.analysis_failed_for_active() {
            // Shown as an error; retrying would only fail again.
        } else if let Some(a) = self.analysis_for(&mesh, &transform) {
            if !self.shows_current_revision() {
                let object = self.active.expect("an active object has a mesh here");
                self.displayed = Some((object, mesh.clone(), a.render.clone()));
            }
        } else if !self
            .requested
            .as_ref()
            .is_some_and(|(m, t)| Mesh::ptr_eq(m, &mesh) && *t == transform)
        {
            self.requested = Some((mesh.clone(), transform));
            // The new render mesh shares this object's chunks on screen.
            let previous = self
                .displayed
                .as_ref()
                .filter(|(object, _, _)| Some(*object) == self.active)
                .map(|(_, _, render)| render.clone());
            let object = self.active.expect("an active object has a mesh here");
            let inputs = self.state_inputs(object, &mesh);
            self.state_seq += 1;
            self.analyzer.submit(worker::AnalysisRequest {
                mesh,
                transform,
                previous,
                states: Some((inputs, self.state_seq)),
            });
        }
        if self.camera.is_none() && self.view_request.is_none() {
            self.view_request = Some(ViewRequest::Reset);
        }
    }

    /// Request render vertices for every object that isn't active
    /// (MF-ISSUE-042): the viewport draws them dimmed at their transform,
    /// but they never need the active object's full diagnostics.
    fn refresh_object_vertices(&mut self) {
        let stale: Vec<(ObjectId, Mesh)> = self
            .session
            .document()
            .objects()
            .iter()
            .filter(|o| Some(o.id) != self.active)
            .filter_map(|o| {
                let mesh = o.mesh()?;
                let have = self
                    .object_vertices
                    .get(&o.id)
                    .is_some_and(|(m, _)| Mesh::ptr_eq(m, mesh));
                let requested = self
                    .requested_vertices
                    .get(&o.id)
                    .is_some_and(|m| Mesh::ptr_eq(m, mesh));
                (!have && !requested).then(|| (o.id, mesh.clone()))
            })
            .collect();
        for (id, mesh) in stale {
            self.requested_vertices.insert(id, mesh.clone());
            let previous = self.object_vertices.get(&id).map(|(_, r)| r.clone());
            self.workers.build_vertices(id, mesh, previous);
        }
        // Drop vertices and in-flight requests for objects no longer in the document.
        let doc = self.session.document();
        self.object_vertices
            .retain(|id, _| doc.object(*id).is_some());
        self.requested_vertices
            .retain(|id, _| doc.object(*id).is_some());
    }

    /// Every object the viewport should draw this frame: the active one
    /// (its render mesh comes from the full analysis, so it can lag
    /// behind while a new revision is analysed: that object's previous
    /// revision stays on screen, never another object's) plus every other
    /// object whose render mesh is ready (MF-ISSUE-042).
    pub fn object_views(&self) -> Vec<ObjectView> {
        let doc = self.session.document();
        doc.objects()
            .iter()
            .filter_map(|o| {
                let active = Some(o.id) == self.active;
                let render = if active {
                    // `displayed` may still hold a previously active
                    // object's render mesh until this one's analysis
                    // returns (review I4): never draw that at this
                    // object's transform.
                    let (object, _, r) = self.displayed.as_ref()?;
                    (*object == o.id).then(|| r.clone())?
                } else {
                    self.object_vertices.get(&o.id)?.1.clone()
                };
                let hovered = !active
                    && self.shown.hover
                    && self
                        .hover
                        .as_ref()
                        .is_some_and(|(h, t)| *h == o.id && *t == OverlayTarget::Object);
                Some(ObjectView {
                    id: o.id,
                    render,
                    transform: o.transform,
                    active,
                    hovered,
                    // Only the active object's current revision has its
                    // states: the overlays belong to it. Another object
                    // shows only its hover glow (its row hovered).
                    shown_states: if active && self.shows_current_revision() {
                        self.shown_states()
                    } else if hovered {
                        u32::from(FaceStates::HOVER)
                    } else {
                        0
                    },
                })
            })
            .collect()
    }

    /// An object's bounds (object space), from vertices built on a worker:
    /// `None` until they're ready, `Some(None)` for an empty mesh.
    fn object_bounds(&self, o: &mf_command::Object) -> Option<Option<Aabb>> {
        let mesh = o.mesh()?;
        let vertices = if Some(o.id) == self.active {
            &self.analysis_for(mesh, &o.transform)?.render
        } else {
            let (for_mesh, v) = self.object_vertices.get(&o.id)?;
            if !Mesh::ptr_eq(for_mesh, mesh) {
                return None;
            }
            v
        };
        Some(vertices.bounds)
    }

    /// Union of every object's bounding box, at its transform
    /// (MF-ISSUE-042): what the camera fits. `None` until every object's
    /// bounds are known; `Some(None)` if there's nothing to frame. O(objects).
    fn document_world_bounds(&self) -> Option<Option<Aabb>> {
        let mut out: Option<Aabb> = None;
        for o in self.session.document().objects() {
            if o.mesh().is_none() {
                continue;
            }
            if let Some(bb) = self.object_bounds(o)? {
                let world = transformed_bounds(&bb, &model_matrix(&o.transform));
                out = Some(match out {
                    None => world,
                    Some(acc) => union_bounds(&acc, &world),
                });
            }
        }
        Some(out)
    }

    /// Home: default view of everything (applied once every object's bounds
    /// are known).
    pub fn fit_camera(&mut self) {
        self.stop_finding();
        self.view_request = Some(ViewRequest::Reset);
    }

    /// Drop a pending or running framing from the tree (another camera
    /// request replaces it).
    fn stop_finding(&mut self) {
        self.find = None;
        self.moving = None;
        self.pulse = None;
    }

    /// F: frame the current target, or everything, keeping the orientation.
    pub fn frame_target(&mut self) {
        self.stop_finding();
        self.view_request = Some(if self.target_overlay_request().is_some() {
            ViewRequest::FrameTarget
        } else {
            ViewRequest::FrameAll
        });
    }

    /// Apply a pending camera move once what it needs is ready.
    fn resolve_view_request(&mut self) {
        let Some(request) = self.view_request else {
            return;
        };
        let bounds = match request {
            ViewRequest::Reset | ViewRequest::FrameAll => match self.document_world_bounds() {
                Some(b) => b,
                None => return,
            },
            ViewRequest::FrameTarget => {
                let Some((object, mesh, target)) = self.target_overlay_request() else {
                    // The target went away: frame everything instead of nothing.
                    self.view_request = Some(ViewRequest::FrameAll);
                    return;
                };
                // Only an overlay built for this exact target will do.
                let Some(slot) = self.overlays.get(&OverlayPurpose::Selection).filter(|s| {
                    s.object == object && Mesh::ptr_eq(&s.mesh, &mesh) && s.target == target
                }) else {
                    return;
                };
                let Some(overlay) = &slot.ready else { return };
                let Some(o) = self.session.document().object(object) else {
                    return;
                };
                let model = model_matrix(&o.transform);
                // Turn to face a target that faces away (a hole on the far
                // side would otherwise be framed from inside the model).
                if let (Some(n), Some(cam)) = (overlay.normal, self.camera.as_mut()) {
                    let world = (glam::DMat3::from_mat4(model).inverse().transpose() * n)
                        .normalize_or_zero();
                    let eye = (cam.eye() - cam.target).normalize_or_zero();
                    if world.dot(eye) < 0.3 {
                        cam.look_from(world);
                    }
                }
                overlay
                    .bounds
                    .map(|bb| inflated(&transformed_bounds(&bb, &model), FRAME_CONTEXT))
            }
        };
        self.view_request = None;
        let scene = self.document_world_bounds().flatten();
        match (bounds, request, &mut self.camera) {
            (None, ViewRequest::Reset, cam) => *cam = None,
            (None, ..) => {}
            (Some(bb), ViewRequest::Reset, cam) | (Some(bb), _, cam @ None) => {
                *cam = Some(OrbitCamera::fit(&bb))
            }
            (Some(bb), _, Some(cam)) => {
                cam.frame(&bb);
                // Framing a speck mustn't clip away the model around it.
                if let Some(scene) = scene {
                    cam.radius = clip_radius(cam.target, cam.radius, &scene);
                }
            }
        }
    }

    /// A row of the tree clicked: select its item (the camera stays).
    pub fn tree_select(&mut self, item: &TreeItem) {
        match item {
            TreeItem::Object(id) => {
                if self.active != Some(*id) {
                    self.tree_hold = Some((self.active, self.now + TREE_HOLD_SECONDS));
                }
                self.set_active(*id);
            }
            TreeItem::Model(id) => {
                self.set_active(*id);
                self.clear_selection();
            }
            TreeItem::Defect(id, d) => {
                self.set_active(*id);
                self.select_defect(d);
            }
        }
    }

    /// A row of the tree double-clicked (or Enter on it): select its item
    /// and frame it, with a short eased camera move
    /// ([`FRAME_MOVE_SECONDS`]) then a pulse of the selection
    /// ([`PULSE_SECONDS`]) so the eye finds it.
    pub fn tree_find(&mut self, item: &TreeItem) {
        self.tree_select(item);
        let mesh = match item {
            TreeItem::Defect(..) => self.active_mesh().cloned(),
            _ => None,
        };
        self.view_request = None;
        self.moving = None;
        self.pulse = None;
        self.find = Some(Find {
            item: item.clone(),
            since: self.now,
            mesh,
        });
    }

    /// The pointer is over a row of the tree this frame: highlight its
    /// item in the view (its outline or glow; the camera stays).
    pub fn tree_hover(&mut self, item: &TreeItem) {
        self.hover = Some(match item {
            TreeItem::Object(id) | TreeItem::Model(id) => (*id, OverlayTarget::Object),
            TreeItem::Defect(id, d) => (*id, defect_target(d)),
        });
    }

    /// The object whose defects the tree lists under its row: the active
    /// one, except for a moment after another object's row was clicked
    /// ([`TREE_HOLD_SECONDS`]).
    pub fn tree_expanded(&self) -> Option<ObjectId> {
        match self.tree_hold {
            Some((held, until)) if self.now < until => held,
            _ => self.active,
        }
    }

    /// The analysis of object `id`'s current revision at its transform, if
    /// it's ready (the active object's is [`Self::current_analysis`]).
    pub fn analysis_of(&self, id: ObjectId) -> Option<&Analysis> {
        let object = self.session.document().object(id)?;
        self.analysis_for(object.mesh()?, &object.transform)
    }

    /// True if a row's item is the current target (the row shows selected).
    pub fn tree_selected(&self, item: &TreeItem) -> bool {
        match item {
            // An object row says which object is active by its text.
            TreeItem::Object(_) => false,
            TreeItem::Model(id) => {
                Some(*id) == self.active
                    && self.selection.is_none()
                    && self.current_inspected().is_none()
            }
            TreeItem::Defect(_, d) => match (d.selection(), self.current_selection()) {
                (Some((kind, id)), Some(sel)) => sel.kind == kind && sel.ids.contains(&id),
                (None, _) => self.current_inspected().is_some_and(|i| i.defect == *d),
                _ => false,
            },
        }
    }

    /// True while the camera moves to (or waits to move to) an item found
    /// from the tree.
    pub fn camera_animating(&self) -> bool {
        self.find.is_some() || self.moving.is_some()
    }

    /// How much the selection is emphasised this frame (0–1): the pulse
    /// after a framing from the tree, else 0.
    pub fn selection_emphasis(&self) -> f32 {
        self.emphasis
    }

    /// Start the camera move for a pending [`Find`] once what it needs is
    /// ready; drop it if its item went away.
    fn resolve_find(&mut self) {
        let Some(find) = self.find.clone() else {
            return;
        };
        if self.now - find.since > FIND_GIVE_UP_SECONDS {
            self.find = None;
            self.status = Some(
                "Couldn't frame it: the view is still being prepared. Try again in a moment."
                    .into(),
            );
            return;
        }
        let Some(cam) = self.camera else { return };
        let scene = match self.document_world_bounds() {
            None => return,
            Some(None) => {
                self.find = None;
                return;
            }
            Some(Some(scene)) => scene,
        };
        let to = match &find.item {
            TreeItem::Object(id) | TreeItem::Model(id) => {
                let Some(o) = self.session.document().object(*id) else {
                    self.find = None;
                    return;
                };
                match self.object_bounds(o) {
                    None => return,
                    Some(None) => {
                        self.find = None;
                        return;
                    }
                    Some(Some(bb)) => crate::framing::object_view(
                        &cam,
                        &transformed_bounds(&bb, &model_matrix(&o.transform)),
                        &scene,
                    ),
                }
            }
            TreeItem::Defect(id, d) => {
                let current = Some(*id) == self.active
                    && match (&find.mesh, self.active_mesh()) {
                        (Some(m), Some(now)) => Mesh::ptr_eq(m, now),
                        _ => false,
                    };
                if !current || self.analysis_failed_for_active() {
                    self.find = None;
                    return;
                }
                let Some(analysis) = self.current_analysis() else {
                    return;
                };
                let Some(place) = analysis.defects.place(d) else {
                    self.find = None;
                    return;
                };
                if !self.picking_ready() {
                    return;
                }
                crate::framing::defect_view(&cam, &place, &scene, |o, d| self.ray_hit(o, d))
            }
        };
        self.find = None;
        self.moving = Some(Moving {
            path: CameraMove {
                from: cam,
                to,
                start: self.now,
                duration: FRAME_MOVE_SECONDS,
            },
            last: cam,
            // An object's row frames the object: what's selected on it
            // isn't what was asked for, so it doesn't pulse.
            pulse: match find.item {
                TreeItem::Object(_) => None,
                _ => self
                    .target_overlay_request()
                    .map(|(object, _, target)| (object, target)),
            },
        });
    }

    /// The nearest surface of any object along a world-space ray, through
    /// the picking BVHs (O(log n) each; [`Self::resolve_find`] waits until
    /// every object has one).
    fn ray_hit(&self, origin: glam::DVec3, dir: glam::DVec3) -> Option<f64> {
        self.session
            .document()
            .objects()
            .iter()
            .filter_map(|o| {
                let mesh = o.mesh()?;
                let (_, bvh) = self
                    .bvhs
                    .get(&o.id)
                    .filter(|(m, _)| Mesh::ptr_eq(m, mesh))?;
                bvh.ray_pick(
                    mesh,
                    origin.to_array(),
                    dir.to_array(),
                    &model_matrix(&o.transform),
                )
            })
            .map(|(_, t)| t)
            .min_by(f64::total_cmp)
    }

    /// Move the camera along a framing move, then start the pulse. A camera
    /// the user moved meanwhile stops it there.
    fn step_camera_move(&mut self) {
        let Some(moving) = self.moving.as_mut() else {
            return;
        };
        if self.camera != Some(moving.last) {
            self.moving = None;
            return;
        }
        let cam = moving.path.at(self.now);
        moving.last = cam;
        self.camera = Some(cam);
        if moving.path.done(self.now) {
            self.pulse = moving.pulse.take().map(|target| (self.now, target));
            self.moving = None;
        }
    }

    /// The selection's emphasis now: two pulses after a framing from the
    /// tree, while the same target stays selected.
    fn update_emphasis(&mut self) {
        self.emphasis = 0.0;
        let Some((start, target)) = &self.pulse else {
            return;
        };
        let t = self.now - start;
        let same = self
            .target_overlay_request()
            .is_some_and(|(object, _, now)| (object, now) == *target);
        if !same || !(0.0..PULSE_SECONDS).contains(&t) {
            self.pulse = None;
            return;
        }
        let phase = std::f64::consts::PI * t * PULSES / PULSE_SECONDS;
        self.emphasis = phase.sin().powi(2) as f32;
    }

    /// What the selection overlay should show: the selection, or an
    /// inspected tangled edge, on the active object at this revision.
    fn target_overlay_request(&self) -> Option<(ObjectId, Mesh, OverlayTarget)> {
        let mesh = self.active_mesh()?.clone();
        if let Some(sel) = self.current_selection() {
            let target = match sel.kind {
                SelectionKind::BoundaryLoop => OverlayTarget::Loops(sel.ids.clone()),
                SelectionKind::Component => OverlayTarget::Components(sel.ids.clone()),
                SelectionKind::Faces => OverlayTarget::Faces(sel.ids.clone()),
                SelectionKind::Object => return None,
            };
            return Some((sel.object, mesh, target));
        }
        let inspected = self.current_inspected()?;
        match &inspected.defect {
            Defect::Tangled { edge, .. } => {
                Some((inspected.object, mesh, OverlayTarget::Edges(vec![*edge])))
            }
            _ => None,
        }
    }

    /// Faces created by the active object's last few commands
    /// ([`RECENT_REPAIR_COMMANDS`]), as of the current revision. O(faces
    /// created), from the history's provenance; liveness is checked on the
    /// worker that builds the overlay.
    fn recent_repair_faces(&self, object: ObjectId) -> Vec<u32> {
        let mut faces = Vec::new();
        let mut commands = 0;
        let mut cur = Some(self.session.current());
        while let Some(r) = cur {
            let Some(node) = self.session.node(r) else {
                break;
            };
            if node.import.as_ref().is_some_and(|i| i.object == object) {
                break;
            }
            if node.record.as_ref().is_some_and(|rec| rec.target == object) {
                if let Some(p) = node.provenance.get(&object) {
                    let created = p.created_faces.iter().map(|(f, _)| f.0);
                    match &p.remap {
                        // Older faces were renumbered: map this step's and stop.
                        Some(remap) => {
                            faces.extend(created.filter_map(|f| {
                                remap.faces.get(f as usize).copied().flatten().map(|n| n.0)
                            }));
                            break;
                        }
                        None => faces.extend(created),
                    }
                }
                commands += 1;
                if commands == RECENT_REPAIR_COMMANDS {
                    break;
                }
            }
            cur = node.parent;
        }
        faces.sort_unstable();
        faces.dedup();
        faces
    }

    /// Faces made by `object`'s last few commands at its revision `mesh`,
    /// found once per revision.
    fn recent_faces(&mut self, object: ObjectId, mesh: &Mesh) -> Arc<[u32]> {
        if let Some((o, m, faces)) = &self.recent
            && *o == object
            && Mesh::ptr_eq(m, mesh)
        {
            return faces.clone();
        }
        let faces: Arc<[u32]> = self.recent_repair_faces(object).into();
        self.recent = Some((object, mesh.clone(), faces.clone()));
        faces
    }

    /// What the face states of `object`'s revision `mesh` (the active one)
    /// are made from: its selection and hover, its recent repairs and its
    /// tangled edges' faces. Small (O(selection)): the UI thread builds it
    /// every frame.
    fn state_inputs(&mut self, object: ObjectId, mesh: &Mesh) -> StateInputs {
        let selection = self.target_overlay_request();
        let hover = self.hover_request(&selection);
        let faces_of = |want: Option<(ObjectId, Mesh, OverlayTarget)>| {
            want.filter(|(o, m, _)| *o == object && Mesh::ptr_eq(m, mesh))
                .map(|(_, _, t)| t)
                .filter(|t| {
                    matches!(
                        t,
                        OverlayTarget::Components(_)
                            | OverlayTarget::Faces(_)
                            | OverlayTarget::Object
                    )
                })
        };
        StateInputs {
            selected: faces_of(selection),
            hover: faces_of(hover),
            recent: self.recent_faces(object, mesh),
            // Until its analysis is ready (the analysis fills them in).
            tangled: self
                .analyses
                .get(mesh)
                .map_or_else(|| Arc::from([]), |a| a.tangled_faces.clone()),
        }
    }

    /// True if a defect overlay switched on by `on` is drawn (View ▸ Show
    /// defects hides them all).
    fn defect_shown(&self, on: bool) -> bool {
        self.show_boundaries && on
    }

    /// The face-state flags the 3D view shows (the overlays switched on).
    pub fn shown_states(&self) -> u32 {
        let s = &self.shown;
        [
            (s.selection, FaceStates::SELECTED),
            (s.hover, FaceStates::HOVER),
            (
                self.defect_shown(s.loose_pieces),
                FaceStates::SMALL_COMPONENT,
            ),
            (
                self.defect_shown(s.non_manifold),
                FaceStates::NON_MANIFOLD_ADJ,
            ),
            (s.recent_repairs, FaceStates::RECENT_REPAIR),
        ]
        .into_iter()
        .filter(|(on, _)| *on)
        .fold(0, |m, (_, bit)| m | u32::from(bit))
    }

    /// Keep the face states of the active object's shown revision up to
    /// date: when what they're made from changes, a worker rebuilds them
    /// and writes the changed chunks (no geometry is uploaded:
    /// MF-RENDER-003). The UI thread only compares the inputs.
    fn refresh_states(&mut self) {
        if !self.shows_current_revision() {
            return;
        }
        let Some((object, mesh, render)) = self.displayed.clone() else {
            return;
        };
        let inputs = self.state_inputs(object, &mesh);
        if render.wants_states(&inputs) {
            self.state_seq += 1;
            self.workers.write_states(render, inputs, self.state_seq);
        }
        self.refresh_object_hover();
    }

    /// An object that isn't active glows while its row is hovered (its own
    /// face states; written only while hovered and once after).
    fn refresh_object_hover(&mut self) {
        let hovered = self
            .hover
            .as_ref()
            .filter(|(_, t)| *t == OverlayTarget::Object)
            .map(|(o, _)| *o);
        let renders: Vec<(ObjectId, Arc<RenderMesh>)> = self
            .object_vertices
            .iter()
            .filter(|(id, _)| Some(**id) != self.active)
            .map(|(id, (_, r))| (*id, r.clone()))
            .collect();
        for (id, render) in renders {
            let on = hovered == Some(id);
            if !on && !render.hover_requested() {
                continue;
            }
            let inputs = StateInputs {
                selected: None,
                hover: on.then_some(OverlayTarget::Object),
                recent: Arc::from([]),
                tangled: Arc::from([]),
            };
            if render.wants_states(&inputs) {
                self.state_seq += 1;
                self.workers
                    .write_object_states(render, inputs, self.state_seq);
            }
        }
    }

    /// What the pointer is over in the tree (or else in the 3D view),
    /// unless it's the target itself (the pointer is still on the row just
    /// clicked): that shows as selected, not hovered.
    fn hover_request(
        &self,
        selection: &Option<(ObjectId, Mesh, OverlayTarget)>,
    ) -> Option<(ObjectId, Mesh, OverlayTarget)> {
        self.hover
            .clone()
            .or_else(|| self.view_hover().map(|h| (h.object, h.target.clone())))
            .filter(|(object, target)| {
                !selection
                    .as_ref()
                    .is_some_and(|(o, _, t)| o == object && t == target)
            })
            // With nothing targeted the whole active object is the target:
            // hovering it ("Model", its row) shows as selected, not hovered.
            .filter(|(object, target)| {
                !(*target == OverlayTarget::Object
                    && Some(*object) == self.active
                    && selection.is_none())
            })
            .and_then(|(object, target)| {
                let mesh = self.session.document().object(object)?.mesh()?.clone();
                Some((object, mesh, target))
            })
    }

    /// Keep each overlay slot matching what should be shown; request a
    /// build on a worker when it changes. Faces and pieces are tinted by
    /// the face states, so a hover over one needs no overlay.
    fn refresh_overlays(&mut self) {
        let selection = self.target_overlay_request();
        let hover = self
            .hover_request(&selection)
            .filter(|(_, _, t)| matches!(t, OverlayTarget::Loops(_) | OverlayTarget::Edges(_)));
        for (purpose, want) in [
            (OverlayPurpose::Selection, selection),
            (OverlayPurpose::Hover, hover),
        ] {
            // Its lines share the shown render mesh's origin, so they draw
            // exactly over its own outlines (fix round 1, I1); until that
            // object is shown there's nothing to draw it over.
            let origin = want.as_ref().and_then(|(object, ..)| {
                self.displayed
                    .as_ref()
                    .filter(|(o, ..)| o == object)
                    .map(|(.., render)| render.origin)
            });
            let (Some((object, mesh, target)), Some(origin)) = (want, origin) else {
                self.overlays.remove(&purpose);
                continue;
            };
            let same = self.overlays.get(&purpose).is_some_and(|s| {
                s.object == object
                    && Mesh::ptr_eq(&s.mesh, &mesh)
                    && s.target == target
                    && s.origin == origin
            });
            if !same {
                self.workers
                    .build_overlay(purpose, object, mesh.clone(), target.clone(), origin);
                self.overlays.insert(
                    purpose,
                    OverlaySlot {
                        object,
                        mesh,
                        target,
                        origin,
                        ready: None,
                    },
                );
            }
        }
    }

    /// The 3D view's line overlays, bottom to top (theme.rs gives each its
    /// colour). Face tints are the render mesh's face states instead.
    pub fn view_layers(&self) -> Vec<ViewLayer> {
        let mut layers = Vec::new();
        let lines = |object, origin, lines: &Arc<mf_render::GpuLines>, color| ViewLayer {
            object,
            origin,
            lines: lines.clone(),
            color,
            xray: false,
            emphasis: 0.0,
        };
        let ready = |p| {
            self.overlays
                .get(&p)
                .and_then(|s| Some((s.object, s.ready.as_ref()?)))
        };
        if let Some((object, a)) = self.active.zip(self.current_analysis()) {
            if self.defect_shown(self.shown.cracks) {
                layers.push(lines(
                    object,
                    a.render.origin,
                    &a.layers.cracks,
                    theme::CRACK,
                ));
            }
            if self.defect_shown(self.shown.non_manifold) {
                layers.push(lines(
                    object,
                    a.render.origin,
                    &a.layers.tangled,
                    theme::NON_MANIFOLD,
                ));
            }
        }
        if self.shown.selection
            && let Some((object, o)) = ready(OverlayPurpose::Selection)
        {
            // Where it's hidden it shows through, faintly (x-ray).
            layers.push(ViewLayer {
                xray: true,
                emphasis: self.emphasis,
                ..lines(object, o.origin, &o.lines, theme::SELECTED)
            });
        }
        if self.shown.hover
            && let Some((object, o)) = ready(OverlayPurpose::Hover)
        {
            layers.push(lines(object, o.origin, &o.lines, theme::HOVER));
        }
        // Overlays are built for the current revision: while the active
        // object still shows its previous one, they wouldn't line up.
        let stale = (!self.shows_current_revision())
            .then_some(self.active)
            .flatten();
        layers.retain(|l| Some(l.object) != stale && !l.lines.is_empty());
        layers
    }

    /// Mesh geometry bytes (surfaces and outlines) uploaded by every
    /// render-mesh build so far (diagnostics and tests: a selection change
    /// must not add to it).
    pub fn geometry_bytes_uploaded(&self) -> u64 {
        self.gpu_totals.geometry_uploaded()
    }

    /// The face states the GPU holds for `face` of the active object's
    /// shown revision (`None`: no such live face, or nothing shown). Reads
    /// the GPU back and waits: for tests and diagnostics only.
    pub fn read_face_state(&self, face: u32) -> Result<Option<u8>, mf_render::RenderError> {
        let Some((object, mesh, render)) = &self.displayed else {
            return Ok(None);
        };
        if Some(*object) != self.active {
            return Ok(None);
        }
        let Some((chunk, drawn)) = FaceStates::locate(mesh, mf_geometry::FaceId(face)) else {
            return Ok(None);
        };
        let (device, queue) = self.viewport.device();
        let words = render.gpu.read_face_states(device, queue, chunk)?;
        Ok(words.get(drawn / 4).map(|w| (w >> (8 * (drawn % 4))) as u8))
    }

    /// Like [`Self::read_face_state`], for any object drawn (an object
    /// that isn't active has its own face states while its row is
    /// hovered). Reads the GPU back and waits: tests and diagnostics only.
    pub fn read_object_face_state(
        &self,
        object: ObjectId,
        face: u32,
    ) -> Result<Option<u8>, mf_render::RenderError> {
        if Some(object) == self.active {
            return self.read_face_state(face);
        }
        let Some((mesh, render)) = self.object_vertices.get(&object) else {
            return Ok(None);
        };
        let Some((chunk, drawn)) = FaceStates::locate(mesh, mf_geometry::FaceId(face)) else {
            return Ok(None);
        };
        let (device, queue) = self.viewport.device();
        let words = render.gpu.read_face_states(device, queue, chunk)?;
        Ok(words.get(drawn / 4).map(|w| (w >> (8 * (drawn % 4))) as u8))
    }

    /// True when the 3D view draws the active object's current revision
    /// (not an earlier one while the current one is being prepared).
    pub fn shows_current_revision(&self) -> bool {
        match (&self.displayed, self.active_mesh()) {
            (Some((object, shown, _)), Some(current)) => {
                Some(*object) == self.active && Mesh::ptr_eq(shown, current)
            }
            _ => false,
        }
    }

    /// True while the active revision's analysis isn't ready (and hasn't
    /// failed).
    pub fn is_analyzing(&self) -> bool {
        self.active_mesh().is_some()
            && self.current_analysis().is_none()
            && !self.analysis_failed_for_active()
    }

    /// True if the active revision's analysis failed.
    pub fn analysis_failed_for_active(&self) -> bool {
        let Some(o) = self
            .active
            .and_then(|id| self.session.document().object(id))
        else {
            return false;
        };
        match (&self.analysis_failed, o.mesh()) {
            (Some((m, t)), Some(mesh)) => Mesh::ptr_eq(m, mesh) && *t == o.transform,
            _ => false,
        }
    }
}

impl eframe::App for MeshFixerApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let started = Instant::now();
        let frame = self.frame_times.begin(started);
        for (index, gpu) in self.viewport.collect_gpu_times() {
            match gpu {
                Some(time) => self.frame_times.set_gpu(index, time),
                None => self.frame_times.set_gpu_unusable(index),
            }
        }
        self.frame(ui, frame);
        let gpu = self.viewport.frame_gpu_time(frame);
        self.frame_times.end(frame, started.elapsed(), gpu);
    }
}

impl MeshFixerApp {
    /// One frame of the UI; `frame_index` is its index in `frame_times`.
    fn frame(&mut self, ui: &mut egui::Ui, frame_index: u64) {
        let ctx = ui.ctx().clone();
        while let Ok(msg) = self.rx.try_recv() {
            self.handle(msg);
        }
        self.refresh_analysis();
        self.refresh_object_vertices();
        self.refresh_bvhs();
        self.resolve_view_request();
        self.now = ctx.input(|i| i.time);

        // Shortcuts go to a focused text field first (MF-ISSUE-009).
        // …and to an open popup or menu.
        let typing = ctx.memory(|m| m.focused().is_some()) || egui::Popup::is_any_open(&ctx);
        let (undo, redo, frame, home, escape, zoom) = ctx.input_mut(|i| {
            if typing {
                return (false, false, false, false, false, 0);
            }
            let escape = i.key_pressed(egui::Key::Escape);
            let redo = i.consume_key(
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                egui::Key::Z,
            );
            let undo = i.consume_key(egui::Modifiers::COMMAND, egui::Key::Z);
            let frame = i.key_pressed(egui::Key::F);
            // Cmd/Ctrl+0 resets too: Home needs Fn+← on Mac laptops.
            let home = i.consume_key(egui::Modifiers::COMMAND, egui::Key::Num0)
                || i.key_pressed(egui::Key::Home);
            let zoom_in = i.consume_key(egui::Modifiers::NONE, egui::Key::Plus)
                || i.consume_key(egui::Modifiers::SHIFT, egui::Key::Plus)
                || i.consume_key(egui::Modifiers::NONE, egui::Key::Equals);
            let zoom_out = i.consume_key(egui::Modifiers::NONE, egui::Key::Minus);
            (
                undo,
                redo,
                frame,
                home,
                escape,
                i32::from(zoom_in) - i32::from(zoom_out),
            )
        });
        // Esc stops a running job first; otherwise it clears the selection.
        if escape {
            if self.can_cancel() {
                self.cancel_job();
            } else {
                self.clear_selection();
            }
        }
        if redo {
            self.redo();
        } else if undo {
            self.undo();
        }
        if !ctx.egui_wants_keyboard_input() {
            // + / = and − zoom toward the view centre, one step per press.
            if zoom != 0
                && let Some(cam) = self.camera.as_mut()
            {
                cam.zoom(ZOOM_STEP.powi(-zoom));
            }
            if home {
                self.fit_camera();
            } else if frame {
                self.frame_target();
            }
        }
        self.resolve_view_request();
        self.resolve_find();
        self.step_camera_move();
        self.update_emphasis();

        self.hover = None;
        crate::panels::menu_bar(self, ui);
        crate::panels::error_banner(self, ui);
        crate::panels::history_bar(self, ui);
        crate::panels::tree_panel(self, ui);
        crate::panels::properties_panel(self, ui);
        crate::panels::replay_window(self, &ctx);
        crate::panels::controls_window(self, &ctx);
        self.refresh_overlays();
        self.refresh_states();

        let objects = self.object_views();
        let placeholder = match self.active_mesh() {
            Some(m) if m.is_empty() => "Empty mesh (0 triangles)",
            Some(_) if self.analysis_failed_for_active() => "Couldn't analyse this object",
            Some(_) => "Analyzing…",
            None => "Open a mesh with File > Open (STL, OBJ or 3MF)",
        };
        let show_boundaries = self.defect_shown(self.shown.holes);
        let updating = self.is_analyzing();
        let layers = self.view_layers();
        // Selected faces show through what hides them (x-ray); an outline
        // carries its own x-ray (`view_layers`).
        let xray_faces = self.shown.selection
            && self.target_overlay_request().is_some_and(|(.., t)| {
                matches!(t, OverlayTarget::Components(_) | OverlayTarget::Faces(_))
            });
        let cue = SelectionCue {
            emphasis: self.emphasis,
            xray_faces,
        };
        let out = egui::CentralPanel::no_frame()
            .show(ui, |ui| {
                self.viewport.show(
                    ui,
                    &mut self.camera,
                    &objects,
                    ViewOptions {
                        show_boundaries,
                        layers,
                        updating,
                        placeholder,
                        frame: frame_index,
                        cue,
                    },
                )
            })
            .inner;
        match out.action {
            Some(NavAction::FrameSelection) => self.frame_target(),
            Some(NavAction::ResetView) => self.fit_camera(),
            None => {}
        }
        if let Some(c) = out.click
            && self.busy.is_none()
        {
            self.click(c);
        }
        self.update_hover(out.pointer);
        if self.hover.is_none() {
            crate::panels::view_hover_tooltip(self, &ctx);
        }

        let overlays_pending = self.overlays.values().any(|s| s.ready.is_none());
        if self.busy.is_some()
            || self.is_analyzing()
            || self.view_request.is_some()
            || self.camera_animating()
            || self.pulse.is_some()
            || self.tree_hold.is_some_and(|(_, until)| self.now < until)
            || overlays_pending
        {
            ctx.request_repaint();
        }
    }
}

/// An operation's report in plain words for the status line. A 5M fill's
/// raw report lists hundreds of loops: laying that out took ~35 ms of a
/// frame, and it isn't for people anyway.
fn plain_report(op_id: &str, report: &serde_json::Value) -> String {
    let len = |key: &str| report[key].as_array().map_or(0, Vec::len);
    // allow-swallow: the operation's own report; a count it leaves out reads as 0
    let num = |key: &str| report[key].as_u64().unwrap_or_default();
    let s = |n: u64| if n == 1 { "" } else { "s" };
    match op_id {
        "mesh.fill_holes" => {
            let (filled, skipped) = (len("filled_loops") as u64, len("skipped_loops") as u64);
            let faces = num("created_faces");
            let mut text = format!(
                "Filled {filled} hole{} with {faces} new face{}",
                s(filled),
                s(faces)
            );
            if skipped > 0 {
                text.push_str(&format!(
                    "; {skipped} left open (too large or can't be filled)"
                ));
            }
            text
        }
        "mesh.remove_small_components" => {
            let (pieces, tris) = (num("removed_components"), num("removed_triangles"));
            format!(
                "Removed {pieces} piece{} ({tris} triangle{})",
                s(pieces),
                s(tris)
            )
        }
        "mesh.compact" => {
            let (faces, vertices) = (num("reclaimed_faces"), num("reclaimed_vertices"));
            format!(
                "Reclaimed unused space: {faces} face slot{} and {vertices} vertex slot{}",
                s(faces),
                s(vertices)
            )
        }
        _ => {
            let raw = report.to_string();
            match raw.char_indices().nth(160) {
                Some((at, _)) => format!("{}…", &raw[..at]),
                None => raw,
            }
        }
    }
}

/// The start of the status after a job that finished before Cancel took
/// effect (its result was kept).
const TOO_LATE: &str = "Finished before it could be cancelled";

/// The axis-aligned box containing every corner of `bb` after `model`
/// (MF-ISSUE-042): a rotation or mirror can move the AABB's own min/max off
/// the transformed shape's extremes, so every corner is transformed.
fn transformed_bounds(bb: &Aabb, model: &glam::DMat4) -> Aabb {
    let corners = (0u8..8).map(|i| {
        let pick = |lo: f64, hi: f64, bit: u8| if i & bit == 0 { lo } else { hi };
        glam::DVec3::new(
            pick(bb.min[0], bb.max[0], 1),
            pick(bb.min[1], bb.max[1], 2),
            pick(bb.min[2], bb.max[2], 4),
        )
    });
    let mut out: Option<Aabb> = None;
    for c in corners {
        let p = (*model * glam::DVec4::from((c, 1.0))).truncate().to_array();
        out = Some(match out {
            None => Aabb { min: p, max: p },
            Some(mut acc) => {
                for (k, &v) in p.iter().enumerate() {
                    acc.min[k] = acc.min[k].min(v);
                    acc.max[k] = acc.max[k].max(v);
                }
                acc
            }
        });
    }
    // `bb` is never empty for a non-empty mesh's bounding box, so there's
    // always at least one corner.
    out.expect("a bounding box has 8 corners")
}

/// What hovering or selecting `d` outlines or tints.
pub fn defect_target(d: &Defect) -> OverlayTarget {
    match d {
        Defect::Hole { loop_id, .. }
        | Defect::OverCap { loop_id, .. }
        | Defect::Crack { loop_id, .. } => OverlayTarget::Loops(vec![*loop_id]),
        Defect::LoosePiece { component, .. } => OverlayTarget::Components(vec![*component]),
        Defect::Tangled { edge, .. } => OverlayTarget::Edges(vec![*edge]),
    }
}

/// Why a selection couldn't follow a history move, in plain words.
fn selection_lost(e: &TranslateError) -> String {
    let why = match e {
        TranslateError::NoPath => "that step is on another branch",
        TranslateError::ObjectMissing => "its object doesn't exist at this step",
        TranslateError::AllLost { .. } => "what was selected doesn't exist at this step",
        TranslateError::LoopClosed { .. } => "the hole is closed at this step",
        TranslateError::KindNotTranslatable => "this kind of selection can't follow history",
        TranslateError::NotInSource { .. } => "it named elements that don't exist",
    };
    format!("Selection cleared: {why}")
}

/// How much bigger than the target `F` frames, so its surroundings show.
const FRAME_CONTEXT: f64 = 2.5;

/// `bb` scaled by `factor` about its centre.
fn inflated(bb: &Aabb, factor: f64) -> Aabb {
    let c = bb.center();
    let half = |k: usize| (bb.max[k] - bb.min[k]) * 0.5 * factor;
    Aabb {
        min: [0, 1, 2].map(|k| c[k] - half(k)),
        max: [0, 1, 2].map(|k| c[k] + half(k)),
    }
}

/// Default value of an integer parameter of a built-in operation.
fn spec_default(op_id: &str, param: &str) -> i64 {
    let spec = mf_spec::builtin::get(op_id).expect("a built-in operation");
    let p = spec
        .parameters
        .iter()
        .find(|p| p.name == param)
        .expect("the built-in operation declares this parameter");
    match p.kind.default_value() {
        ParamValue::Int(v) => v,
        other => panic!("{op_id}.{param} is an integer parameter, got {other:?}"),
    }
}

/// A history step in plain words: "Import part", "Fill hole", "Remove piece".
fn step_label(node: &mf_command::RevisionNode) -> String {
    if let Some(import) = &node.import {
        return format!("Import {}", import.name);
    }
    node.record
        .as_ref()
        .and_then(|rec| {
            let target = rec.selection.as_ref().map(|s| (s.kind, s.ids.len()));
            operation_label(&rec.op_id, target)
        })
        // allow-swallow: a step with no plain words keeps the session's own label
        .unwrap_or_else(|| node.label.clone())
}

fn union_bounds(a: &Aabb, b: &Aabb) -> Aabb {
    let mut out = *a;
    for k in 0..3 {
        out.min[k] = out.min[k].min(b.min[k]);
        out.max[k] = out.max[k].max(b.max[k]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An operation's report reaches the status line in plain words, and
    /// anything else is kept short (a 5M fill's raw report is huge).
    #[test]
    fn reports_are_summarised_in_plain_words() {
        let fill = serde_json::json!({
            "created_faces": 10,
            "filled_loops": [{"id": 1}, {"id": 2}],
            "skipped_loops": [{"id": 3}],
        });
        assert_eq!(
            plain_report("mesh.fill_holes", &fill),
            "Filled 2 holes with 10 new faces; 1 left open (too large or can't be filled)"
        );
        let removed = serde_json::json!({"removed_components": 1, "removed_triangles": 12});
        assert_eq!(
            plain_report("mesh.remove_small_components", &removed),
            "Removed 1 piece (12 triangles)"
        );
        let big = serde_json::json!({ "values": (0..1000).collect::<Vec<_>>() });
        let text = plain_report("mesh.inspect", &big);
        assert!(text.chars().count() <= 161 && text.ends_with('…'), "{text}");
    }

    /// The status line explains a lost selection in words, never a debug dump.
    #[test]
    fn lost_selection_is_explained_in_plain_words() {
        for e in [
            TranslateError::NoPath,
            TranslateError::ObjectMissing,
            TranslateError::AllLost { lost: vec![1] },
            TranslateError::LoopClosed { faces: vec![] },
            TranslateError::KindNotTranslatable,
            TranslateError::NotInSource { ids: vec![2] },
        ] {
            let text = selection_lost(&e);
            assert!(text.starts_with("Selection cleared: "), "{text}");
            assert!(
                !text.contains('{') && !text.contains('[') && !text.contains("Lost"),
                "{text}"
            );
        }
    }
}
