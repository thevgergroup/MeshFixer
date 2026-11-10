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
use crate::defects::{Defect, HoleNumbers, HoleView, count, operation_label, thousands};
use crate::import_dialog::UnitPrompt;
use crate::metrics::{JobStart, MetricsSnapshot, RecentJobs, Rss};
use crate::theme;
use crate::tools::{Interactive, LeftTab, LegendRow, ToolGroup, ToolKind};
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
    /// Ask for the unit of a unitless file (STL, OBJ) when it's opened
    /// (MF-ISSUE-045): the app as people launch it does. Off, such a file
    /// reads as millimetres with a warning saying so, as the CLI does
    /// without `--units` (tests and scripted use).
    pub unit_prompt: bool,
    /// GPU memory cached revisions may hold beyond the current revisions'
    /// (`None`: [`crate::analysis::DEFAULT_EXTRA_GPU_BYTES`], 1 GiB; M4
    /// final review I5).
    pub gpu_cache_extra_bytes: Option<u64>,
}

/// How the running job (the one the status bar shows) can be stopped.
struct RunningJob {
    /// `None`: this kind of job can't be cancelled (replay).
    run: Option<RunContext>,
    cancel_requested: bool,
    /// When and at what heap level it started (View ▸ Metrics).
    start: JobStart,
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
    /// One-shot (parameters and Apply) or interactive (works in the view).
    pub kind: ToolKind,
    /// Its group in Properties' whole-object list (`None`: a targeted tool).
    pub group: Option<ToolGroup>,
}

/// An Apply waiting for the user's go-ahead (MF-ISSUE-052).
#[derive(Debug, Clone)]
struct Confirm {
    tool: Tool,
    message: String,
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
/// rather than wait (and redraw) forever. Wall-clock seconds: what it
/// waits for is background work, which runs in wall time; input time runs
/// ahead of it whenever frames come faster than real time (M4 final
/// review I4).
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
    /// When it was asked for (wall clock): it gives up
    /// [`FIND_GIVE_UP_SECONDS`] later.
    asked: Instant,
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
    /// View ▸ Metrics is open.
    pub show_metrics: bool,
    /// What it shows, refreshed every [`crate::metrics::REFRESH_EVERY`]
    /// while open.
    metrics: Option<MetricsSnapshot>,
    /// The process RSS, sampled off the UI thread while the panel is open.
    rss: Rss,
    /// The last few finished jobs: duration and heap peak.
    recent_jobs: RecentJobs,
    /// The whole-object tool open in Properties (its catalogue id,
    /// `tools::CATALOGUE`).
    pub tool: Option<String>,
    /// The interactive tool open, if one is: it owns the view's primary
    /// drag and its keys, and supplies the legend (`tools::Interactive`).
    pub interactive: Option<Interactive>,
    /// The left panel's section shown (`tools::LeftTab`).
    pub left_tab: LeftTab,
    pub op_params: BTreeMap<String, BTreeMap<String, ParamValue>>,
    pub show_boundaries: bool,
    pub error: Option<String>,
    pub status: Option<String>,
    /// Every warning from the latest import (MF-ISSUE-004), not just the last.
    pub warnings: Vec<String>,
    /// The last command's result when it carried a warning (MF-ISSUE-055's
    /// curvature limit): shown in full at the top of Properties until
    /// dismissed or the next command finishes, so neither a later status
    /// message nor the status line's width can hide it (M4 final UX I1).
    pub notice: Option<String>,
    /// View > Import warnings: the last import's warnings, grouped (I2).
    pub show_warnings: bool,
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
    /// Asks for the unit of unitless files ([`Settings::unit_prompt`]).
    ask_units: bool,
    /// A unitless file read and waiting for its unit.
    unit_prompt: Option<UnitPrompt>,
    /// An Apply waiting for confirmation.
    confirm: Option<Confirm>,
    /// Each object's hole numbering so far (addendum C), from its latest
    /// analysis.
    hole_numbers: BTreeMap<ObjectId, Arc<HoleNumbers>>,
    /// The holes listed for the active revision after a mark changed (the
    /// analysis has the list for the marks it was asked with).
    hole_list: Option<(Arc<crate::defects::Defects>, Arc<HoleView>)>,
    /// The last list asked for: (revision, marks).
    hole_list_requested: Option<(Arc<crate::defects::Defects>, Vec<u32>)>,
    /// View ▸ Show hole overview: a faint tint around every hole and a
    /// marker at each one too small to see.
    pub show_hole_overview: bool,
    /// A marker on each listed hole too small to see at this zoom (on by
    /// default, so the view shows where the holes are: M4 final UX I6).
    /// View > Show hole overview shows them too, with its tint.
    pub show_hole_markers: bool,
    /// The side panels (View > Hide side panels, Cmd/Ctrl+\, or each
    /// panel's « / »): hidden, the view has the window (M4 final UX I5).
    pub show_tree: bool,
    pub show_properties: bool,
    /// Works out the overview's markers on the background pool.
    marker_worker: LatestWorker<worker::MarkerRequest>,
    /// What the markers were last asked for, and its number.
    marker_query: Option<(MarkerQuery, u64)>,
    /// The latest markers (request number, markers).
    markers: Option<(u64, Arc<worker::Markers>)>,
    /// A hole (loop id) N / Shift+N selected: the tree scrolls to its row.
    reveal: Option<u32>,
}

/// What the hole overview's markers depend on: a change asks again.
#[derive(Debug, Clone, Copy, PartialEq)]
struct MarkerQuery {
    camera: OrbitCamera,
    rect: egui::Rect,
    defects: usize,
    view: usize,
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
        let marker_worker = {
            let (tx, ctx) = (tx.clone(), cc.egui_ctx.clone());
            LatestWorker::new(
                workers.background(),
                |req: worker::MarkerRequest, run: &RunContext| {
                    Ok((req.key, Arc::new(worker::markers(&req, run)?)))
                },
                move |(key, markers)| {
                    // allow-swallow: the receiver only disappears while the app is closing
                    let _ = tx.send(Message::Markers { key, markers });
                    ctx.request_repaint();
                },
            )
        };
        let mut app = MeshFixerApp {
            session: Session::new(),
            active: None,
            camera: None,
            analyses: AnalysisCache::new(
                settings
                    .gpu_cache_extra_bytes
                    .unwrap_or(crate::analysis::DEFAULT_EXTRA_GPU_BYTES),
            ),
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
            show_metrics: false,
            metrics: None,
            rss: Rss::default(),
            recent_jobs: RecentJobs::default(),
            tool: None,
            interactive: None,
            left_tab: LeftTab::default(),
            op_params: BTreeMap::new(),
            show_boundaries: true,
            error: None,
            status: None,
            warnings: Vec::new(),
            notice: None,
            show_warnings: false,
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
            ask_units: settings.unit_prompt,
            unit_prompt: None,
            confirm: None,
            hole_numbers: BTreeMap::new(),
            hole_list: None,
            hole_list_requested: None,
            show_hole_overview: false,
            show_hole_markers: true,
            show_tree: true,
            show_properties: true,
            marker_worker,
            marker_query: None,
            markers: None,
            reveal: None,
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
        self.show_warnings = false;
        if !self.warnings.is_empty() {
            self.status = Some(format!(
                "Imported with {}",
                count(self.warnings.len(), "warning")
            ));
        }
        match ids.first() {
            Some(&id) => {
                self.set_active(id);
                // Refit even into a non-empty document (MF-ISSUE-042
                // review I5): otherwise the camera keeps framing the old
                // document and the newly imported object can be off screen.
                self.fit_camera();
            }
            None => {
                self.error = Some(
                    "The file contains no mesh objects. Open a file with at least one mesh \
                     (STL, OBJ or 3MF)."
                        .into(),
                )
            }
        }
    }

    /// A file was read: a unitless one (STL, OBJ) asks for its unit first
    /// (MF-ISSUE-045) when the app asks; otherwise it's added now. Without
    /// the prompt it reads as millimetres, with a warning saying so.
    fn imported(&mut self, mut model: ImportedModel, extent: Option<[f64; 3]>) {
        if model.source.format == Format::ThreeMf {
            self.import_model(model);
            return;
        }
        match extent {
            Some(extent) if self.ask_units => {
                let name = model
                    .source
                    .path
                    .file_name()
                    .map_or_else(|| "the file".into(), |n| n.to_string_lossy().into_owned());
                self.unit_prompt = Some(UnitPrompt::new(name, extent, model));
            }
            _ => {
                model.warnings.push(format!(
                    "{} has no units; read as millimetres",
                    model.source.path.display()
                ));
                self.import_model(model);
            }
        }
    }

    /// The unitless file waiting for its unit, if one is.
    pub fn unit_prompt(&self) -> Option<&UnitPrompt> {
        self.unit_prompt.as_ref()
    }

    /// Show the unit dialog while a file waits for its unit, and act on
    /// the answer.
    fn unit_dialog(&mut self, ctx: &egui::Context) {
        let Some(prompt) = self.unit_prompt.as_mut() else {
            return;
        };
        match crate::import_dialog::show(prompt, ctx) {
            None => {}
            Some(true) => {
                if let Some(prompt) = self.unit_prompt.take() {
                    self.import_model(prompt.into_model());
                }
            }
            Some(false) => {
                if let Some(prompt) = self.unit_prompt.take() {
                    self.workers.discard(prompt);
                }
                self.status = Some("Import cancelled: nothing opened".into());
            }
        }
    }

    /// Mark a job as running (`label` in the status bar) and return the
    /// context it runs with. `cancellable` jobs get a Cancel button.
    fn start_job(&mut self, label: String, cancellable: bool) -> RunContext {
        let run = RunContext::none();
        self.job = Some(RunningJob {
            run: cancellable.then(|| run.clone()),
            cancel_requested: false,
            start: JobStart::now(&label),
        });
        self.busy = Some(label);
        run
    }

    /// The running job is over: returns whether its cancel was requested.
    fn finish_job(&mut self) -> bool {
        self.busy = None;
        let Some(job) = self.job.take() else {
            return false;
        };
        self.recent_jobs.push(job.start.finish());
        job.cancel_requested
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
            Err(e) => {
                self.error = Some(format!(
                    "Saving the command log failed: {e}. Choose another folder and try again."
                ))
            }
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
        if self.active_mesh().is_none() {
            return;
        }
        let Some(path) = rfd::FileDialog::new()
            .add_filter("3MF (all objects)", &["3mf"])
            .add_filter("Binary STL", &["stl"])
            .add_filter("OBJ", &["obj"])
            .save_file()
        else {
            return;
        };
        self.export_to(path);
    }

    /// Export to `path`, in the format its extension names: 3MF writes
    /// every object with its transform and the document's unit; STL and
    /// OBJ write the active object with its transform applied
    /// (MF-ISSUE-035), and say so in the status once the export finishes.
    pub fn export_to(&mut self, path: PathBuf) {
        let Some(active) = self
            .active
            .and_then(|id| self.session.document().object(id))
        else {
            return;
        };
        let Some(mesh) = active.mesh().cloned() else {
            return;
        };
        let transform = active.transform;
        let unit = self.session.document().units();
        match Format::from_path(&path) {
            Some(Format::ThreeMf) => match self.document_model() {
                Ok(model) => {
                    let run = self.start_job(format!("Exporting {}", path.display()), true);
                    self.workers
                        .export(worker::ExportJob::Model(model), path, run);
                }
                Err(e) => {
                    self.error = Some(format!(
                        "Can't export a 3MF: {e}. Export the mesh objects one at a time as STL or OBJ."
                    ))
                }
            },
            Some(format) => {
                // The transform is applied in the job (O(n), cancellable),
                // never here on the UI thread (M4 final review I3).
                let run = self.start_job(format!("Exporting {}", path.display()), true);
                self.workers.export(
                    worker::ExportJob::Mesh {
                        mesh,
                        transform,
                        format,
                        unit,
                    },
                    path,
                    run,
                );
            }
            None => {
                self.error = Some(format!(
                    "Can't export to {}: use a .3mf, .stl or .obj file name",
                    path.display()
                ))
            }
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
        // A curated list (M4 final UX review I8), never the raw registry:
        // object operations (delete, drop) and strokes have their own
        // places, and a new spec isn't offered until it's listed.
        crate::tools::CATALOGUE
            .iter()
            .map(|e| Tool {
                op_id: e.id.to_owned(),
                label: tool_label(e.id),
                selection: None,
                fixed: BTreeMap::new(),
                kind: e.kind,
                group: Some(e.group),
            })
            .collect()
    }

    /// True when the target is a defect with no operation yet (a crack, a
    /// hole over the fill limit, a pinched hole): Properties says why and
    /// what to do instead of offering whole-object tools that wouldn't fix
    /// it.
    fn target_blocks_whole_object(&self) -> bool {
        let Some((sel, analysis)) = self.current_selection().zip(self.current_analysis()) else {
            return false;
        };
        sel.kind == SelectionKind::BoundaryLoop
            && sel.ids.iter().any(|&id| {
                !analysis
                    .defects
                    .find(sel.kind, id)
                    .is_some_and(|d| analysis.defects.fillable(d))
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
            kind: ToolKind::OneShot,
            group: None,
        };
        // Only holes fill_holes would fill: a pinched one is refused, so it
        // isn't offered (M4 final UX review I7).
        let holes: Option<Vec<usize>> = defects
            .iter()
            .map(|d| match d {
                Defect::Hole { edges, .. } if analysis.defects.fillable(d) => Some(*edges),
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
                // Chosen pieces: the relative limit is for whole-object runs.
                BTreeMap::from([
                    ("max_triangles".into(), ParamValue::Int(largest + 1)),
                    ("max_relative_size".into(), ParamValue::Float(100.0)),
                    ("keep_largest".into(), ParamValue::Bool(true)),
                ]),
            )];
        }
        Vec::new()
    }

    /// Apply, unless it would remove more than 3% of the triangles, or most
    /// of the pieces and more than 2% (MF-ISSUE-052): then it asks first
    /// ([`Self::confirmation`]).
    pub fn apply(&mut self, tool: &Tool) {
        if tool.op_id == "mesh.remove_small_components"
            && tool.selection.is_none()
            && let Some(p) = self.removal_preview()
            && p.needs_confirmation()
        {
            let name = self
                .active
                .and_then(|id| self.session.document().object(id))
                .map_or_else(String::new, |o| o.name.clone());
            let share = 100.0 * p.triangles as f64 / p.of_triangles.max(1) as f64;
            self.confirm = Some(Confirm {
                tool: tool.clone(),
                message: format!(
                    "This would delete {} of {} pieces of '{name}' ({share:.1}% of its \
                     triangles). These may be part of the design, not debris.",
                    thousands(p.pieces),
                    thousands(p.of_pieces),
                ),
            });
            return;
        }
        self.run_tool(tool);
    }

    /// The question Apply is waiting on, if any.
    pub fn confirmation(&self) -> Option<&str> {
        self.confirm.as_ref().map(|c| c.message.as_str())
    }

    /// Answer the question Apply asked: go ahead, or leave it.
    pub fn confirm(&mut self, go: bool) {
        if let Some(c) = self.confirm.take()
            && go
        {
            self.run_tool(&c.tool);
        }
    }

    /// Validate on the UI thread, run on a worker, commit when it returns.
    /// "Fill all holes" fills the listed holes up to its size limit as one
    /// command on those loops: never a marked opening, and one undo.
    fn run_tool(&mut self, tool: &Tool) {
        let Some(target) = self.active else { return };
        // Until the holes are listed (a big model's analysis takes a few
        // seconds) an object with no marked openings fills them as a
        // whole-object run: the same holes, as fill_holes chooses them.
        let listed = self.shared_hole_view().cloned();
        if tool.op_id == "mesh.fill_holes"
            && tool.selection.is_none()
            && (listed.is_some() || !self.active_openings().is_empty())
        {
            let Some(view) = listed else {
                self.status = Some(
                    "Still listing the holes, so the intentional openings can be left open: \
                     try again in a moment"
                        .into(),
                );
                return;
            };
            let limit = match self.params_for("mesh.fill_holes").get("max_loop_edges") {
                Some(ParamValue::Int(n)) => *n,
                other => panic!("fill_holes' max_loop_edges is an integer, got {other:?}"),
            };
            let (k, _) = view.batch(limit.max(0) as usize);
            let ids: Vec<u32> = view.fillable[..k].iter().map(|(_, id)| *id).collect();
            if ids.is_empty() {
                self.status = Some(format!(
                    "No holes of {limit} edges or fewer to fill. Raise \"Fill holes up to\"."
                ));
                return;
            }
            self.fill_loops_up_to(ids, Some(limit), Some(tool.label.clone()));
            return;
        }
        let mut params = self.params_for(&tool.op_id).clone();
        params.extend(tool.fixed.clone());
        let raw_params = params
            .iter()
            .map(|(k, v)| (k.clone(), v.to_string()))
            .collect();
        match self.session.prepare(Invocation {
            operands: Vec::new(),
            selection: tool.selection.clone(),
            op_id: tool.op_id.clone(),
            target,
            raw_params,
        }) {
            Ok(prepared) => {
                let run = self.start_job(format!("{}…", tool.label), true);
                self.workers.run(prepared, run);
            }
            Err(e) => {
                self.error = Some(format!(
                    "{} can't run: {e}. Change the settings and Apply again.",
                    tool.label
                ))
            }
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
        if let Some(note) = self.retarget_selection() {
            self.status = Some(note);
        }
    }

    /// Translate the selection to the current revision (MF-SEL-003). A loop
    /// that was filled clears it with a message; anything not mappable clears
    /// it with the reason. Never applied to a revision it wasn't translated
    /// to. Returns the message, for the caller to show: after a command it
    /// must not replace the command's own result (M4 final UX I1).
    fn retarget_selection(&mut self) -> Option<String> {
        let sel = self.selection.take()?;
        let current = self.session.current();
        if sel.revision == current {
            self.selection = Some(sel);
            return None;
        }
        match self.session.translate(&sel, current) {
            Translation::Mapped(m) => {
                self.selection = Some(m);
                None
            }
            Translation::Partial { mapped, lost } => {
                self.selection = Some(mapped);
                Some(format!(
                    "Selection partly lost ({} no longer at this step)",
                    count(lost.len(), "element")
                ))
            }
            Translation::Failed(e) => Some(selection_cleared(&e)),
            Translation::Ambiguous { candidates } => Some(format!(
                "Selection cleared: it could now be any of {} pieces",
                candidates.len()
            )),
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

    /// Revisions whose analysis (and GPU mesh) is cached.
    pub fn cached_revisions(&self) -> usize {
        self.analyses.len()
    }

    /// GPU memory the cached revisions hold beyond what the current
    /// revisions hold (bounded by `Settings::gpu_cache_extra_bytes`).
    pub fn cache_extra_gpu_bytes(&self) -> u64 {
        let doc = self.session.document();
        self.analyses
            .extra_gpu_bytes(|m| is_current_revision(doc, m))
    }

    /// True while any work handed to the app's pools hasn't finished:
    /// analyses, BVHs, hover picks, face-state writes, jobs. Their answers
    /// arrive in wall time, from other threads, so how many frames pass
    /// meanwhile depends on the machine; tests wait for this to clear
    /// rather than count frames (M4 final review I4).
    pub fn background_work_in_flight(&self) -> bool {
        self.workers.in_flight() > 0 || self.markers_pending()
    }

    /// True while the hole markers asked for haven't come back.
    fn markers_pending(&self) -> bool {
        self.markers_shown()
            && self
                .marker_query
                .is_some_and(|(_, k)| self.markers.as_ref().map(|(m, _)| *m) != Some(k))
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

    /// Esc, in order: stop the running job; else cancel the open
    /// interactive tool's drag; else leave that tool; else clear the
    /// selection. One step per press.
    pub fn escape(&mut self) {
        if self.can_cancel() {
            self.cancel_job();
            return;
        }
        if self.cancel_tool_drag() {
            return;
        }
        if self.interactive.is_some() {
            self.leave_interactive();
            return;
        }
        self.clear_selection();
    }

    /// Cancel the open interactive tool's drag, if one is under way:
    /// nothing is committed. Returns whether there was one.
    pub fn cancel_tool_drag(&mut self) -> bool {
        match self.interactive.as_mut() {
            None => false,
            Some(t) => match *t {},
        }
    }

    /// Open a whole-object tool from Properties (its catalogue id). An
    /// interactive tool starts working in the view; another tool that
    /// was open closes.
    pub fn open_tool(&mut self, id: &str) {
        self.leave_interactive();
        self.tool = Some(id.to_owned());
        self.interactive = crate::tools::Interactive::open(self, id);
    }

    /// Close the whole-object tool open in Properties.
    pub fn close_tool(&mut self) {
        self.leave_interactive();
        self.tool = None;
    }

    /// Close the open interactive tool.
    pub fn leave_interactive(&mut self) {
        if let Some(t) = self.interactive.take()
            && self.tool.as_deref() == Some(t.id())
        {
            self.tool = None;
        }
    }

    /// What the legend lists: the open interactive tool's rows, or what
    /// each overlay colour means.
    pub fn legend_rows(&self) -> Vec<LegendRow> {
        match &self.interactive {
            Some(t) => match *t {},
            None => crate::tools::default_legend(),
        }
    }

    /// The overlay switch a legend row toggles.
    pub fn switch_mut(&mut self, switch: crate::tools::Switch) -> &mut bool {
        use crate::tools::Switch;
        match switch {
            Switch::Selection => &mut self.shown.selection,
            Switch::Hover => &mut self.shown.hover,
            Switch::Holes => &mut self.shown.holes,
            Switch::HoleMarkers => &mut self.show_hole_markers,
            Switch::Cracks => &mut self.shown.cracks,
            Switch::LoosePieces => &mut self.shown.loose_pieces,
            Switch::Tangled => &mut self.shown.non_manifold,
            Switch::RecentRepairs => &mut self.shown.recent_repairs,
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

    /// After a command commits: a created object becomes active (the
    /// command's result, such as a boolean's), and an active object the
    /// command removed hands over to one that exists.
    fn after_object_changes(&mut self) {
        let created = self
            .session
            .node(self.session.current())
            .and_then(|n| n.record.as_ref())
            .and_then(|rec| rec.outputs.iter().find(|o| o.created))
            .map(|o| o.object);
        if let Some(id) = created {
            self.set_active(id);
        }
        self.sync_active();
    }

    /// Delete the active object (the Delete key on an object target): the
    /// `object.delete` command, undoable like any other. Only when nothing
    /// smaller (a hole, a piece, faces) is selected: then Delete would be
    /// ambiguous, so it does nothing.
    pub fn delete_active_object(&mut self) {
        if self.busy.is_some() || self.active.is_none() || self.current_selection().is_some() {
            return;
        }
        self.run_tool(&Tool {
            op_id: "object.delete".into(),
            label: "Delete object".into(),
            selection: None,
            fixed: BTreeMap::new(),
            kind: ToolKind::OneShot,
            group: None,
        });
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
                    Ok((model, extent)) => {
                        self.imported(model, extent);
                        if too_late {
                            self.status = Some(TOO_LATE.into());
                        }
                    }
                    Err(IoError::Cancelled) => self.status = Some(CANCELLED.into()),
                    Err(e) => self.error = Some(import_failed(&e)),
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
                let committed = (*result).and_then(|c| self.session.commit(c));
                // A warning in the result is kept in full until dismissed
                // or the next command finishes (I1).
                self.notice = match &committed {
                    Ok(
                        CommitOutcome::NewRevision { report: r }
                        | CommitOutcome::Report(r)
                        | CommitOutcome::NoChange { report: r },
                    ) if r["warnings"].as_array().is_some_and(|w| !w.is_empty()) => Some(report(r)),
                    _ => None,
                };
                match committed {
                    Ok(CommitOutcome::NewRevision { report: r }) => {
                        self.status = Some(report(&r));
                        // A command can remove, place or create objects
                        // (D6): keep `active` on an object that exists,
                        // and make a newly created one active.
                        self.after_object_changes();
                        // The command's result stays: a filled hole's
                        // "selection cleared" would only repeat it.
                        if let Some(note) = self.retarget_selection()
                            && !note.starts_with("Hole filled")
                        {
                            self.status = Some(match self.status.take() {
                                Some(text) => format!("{text}. {note}"),
                                None => note,
                            });
                        }
                    }
                    Ok(CommitOutcome::Report(r)) => {
                        self.status = Some(report(&r));
                    }
                    Ok(CommitOutcome::NoChange { report: r }) => {
                        self.status = Some(format!("Nothing to change: {}", report(&r)));
                    }
                    Err(CommandError::Cancelled) => self.status = Some(CANCELLED.into()),
                    Err(e) => {
                        self.error = Some(format!(
                            "The operation failed: {e}. Nothing was changed; try other settings \
                             or another target."
                        ))
                    }
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
                self.error = Some(format!(
                    "Internal error in {what}: {message}. Save the command log (File > Save \
                     Command Log) and report it."
                ));
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
                     (internal error: {message}). Undo the last step, or open the file again."
                ));
                self.analysis_failed = Some((mesh, transform));
            }
            Message::HoleView { defects, view } => {
                if let Some(old) = self.hole_list.replace((defects, view)) {
                    self.workers.discard(old);
                }
            }
            Message::BoxPicked {
                object,
                mesh,
                ids,
                add,
            } => {
                let current = self
                    .session
                    .document()
                    .object(object)
                    .and_then(|o| o.mesh())
                    .is_some_and(|m| Mesh::ptr_eq(m, &mesh));
                if current {
                    if ids.is_empty() {
                        if !add {
                            self.clear_selection();
                        }
                    } else {
                        self.set_active(object);
                        self.select(object, SelectionKind::BoundaryLoop, ids, add);
                    }
                }
            }
            Message::Markers { key, markers } => {
                if self.marker_query.is_some_and(|(_, k)| k == key) {
                    self.markers = Some((key, markers));
                }
            }
            Message::Analyzed { mesh, analysis } => {
                // The object's numbering grows with each analysis.
                let object = self
                    .session
                    .document()
                    .objects()
                    .iter()
                    .find(|o| o.mesh().is_some_and(|m| Mesh::ptr_eq(m, &mesh)))
                    .map(|o| o.id);
                if let Some(object) = object {
                    let numbers = &analysis.defects.numbers;
                    let newer = self
                        .hole_numbers
                        .get(&object)
                        .is_none_or(|n| numbers.issued() >= n.issued());
                    if newer {
                        self.hole_numbers.insert(object, numbers.clone());
                    }
                }
                if self
                    .requested
                    .as_ref()
                    .is_some_and(|(m, t)| Mesh::ptr_eq(m, &mesh) && *t == analysis.transform)
                {
                    self.requested = None;
                }
                let doc = self.session.document();
                let evicted = self
                    .analyses
                    .insert(mesh, *analysis, |m| is_current_revision(doc, m));
                if !evicted.is_empty() {
                    self.workers.discard(evicted);
                }
            }
            Message::Replayed(result) => {
                self.finish_job();
                match result {
                    Ok(lines) => self.replay_report = Some(lines),
                    Err(e) => {
                        self.error = Some(format!(
                            "Replay failed: {e}. Check the log's source files are where they were \
                             when it was saved."
                        ))
                    }
                }
            }
            Message::Exported(result) => {
                let too_late = self.finish_job();
                match result {
                    Ok((path, _)) if too_late => {
                        self.status = Some(format!("{TOO_LATE}: exported {}", path.display()))
                    }
                    Ok((path, warnings)) => {
                        // MF-ISSUE-035: STL/OBJ exports apply the object's
                        // transform, which a user can't see from the file
                        // name alone, so say so plainly. MF-ISSUE-034:
                        // empty objects left out of a 3MF export are named.
                        let mut note = format!(
                            "Exported with each object's position applied: {}",
                            path.display()
                        );
                        if !warnings.is_empty() {
                            note.push_str(" (");
                            note.push_str(&warnings.join("; "));
                            note.push(')');
                        }
                        self.status = Some(note);
                    }
                    Err(IoError::Cancelled) => self.status = Some(CANCELLED_EXPORT.into()),
                    Err(e @ IoError::TransformOutOfRange { .. }) => {
                        self.error = Some(format!(
                            "Export failed: {e}. Export as 3MF to keep the transform as it is, \
                             or fix the object's placement in the program that made the file."
                        ))
                    }
                    Err(e) => {
                        self.error = Some(format!(
                            "Export failed: {e}. Choose another location and try again."
                        ))
                    }
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
            if let Some(old) = self.displayed.take() {
                self.workers.discard(old);
            }
            return;
        };
        if self.analysis_failed_for_active() {
            // Shown as an error; retrying would only fail again.
        } else if let Some(render) = self
            .analysis_for(&mesh, &transform)
            .map(|a| a.render.clone())
        {
            if !self.shows_current_revision() {
                let object = self.active.expect("an active object has a mesh here");
                self.analyses.touch(&mesh);
                // The cache may have let go of the revision shown until
                // now (its GPU budget, M4 final review I5): free its
                // buffers off the UI thread.
                if let Some(old) = self.displayed.replace((object, mesh, render)) {
                    self.workers.discard(old);
                }
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
            // allow-swallow: an object with no numbering yet numbers its holes from 1
            let numbers = self.hole_numbers.get(&object).cloned().unwrap_or_default();
            let openings = self.active_openings();
            self.analyzer.submit(worker::AnalysisRequest {
                mesh,
                transform,
                previous,
                states: Some((inputs, self.state_seq)),
                numbers,
                openings,
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
                // F moves there with the same eased move as the tree.
                let from = *cam;
                let mut to = from;
                to.frame(&bb);
                // Framing a speck mustn't clip away the model around it.
                if let Some(scene) = scene {
                    to.radius = clip_radius(to.target, to.radius, &scene);
                }
                self.moving = Some(Moving {
                    path: CameraMove {
                        from,
                        to,
                        start: self.now,
                        duration: FRAME_MOVE_SECONDS,
                    },
                    last: from,
                    pulse: None,
                });
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
            asked: Instant::now(),
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

    /// True while a framing from the tree waits for what it needs (the
    /// item's bounds, the picking BVHs and topology) before its camera
    /// move starts.
    pub fn framing_pending(&self) -> bool {
        self.find.is_some()
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
        if find.asked.elapsed().as_secs_f64() > FIND_GIVE_UP_SECONDS {
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
            loose_limit: self.loose_limit(),
            hole_ring: self
                .show_hole_overview
                .then(|| self.analyses.get(mesh).map(|a| a.hole_ring.clone()))
                .flatten(),
        }
    }

    /// The whole-object "Remove loose pieces" tool's thresholds, while it
    /// is open in Properties (nothing targeted): (max triangles, max share
    /// of the largest piece in %, keep the largest).
    fn removal_thresholds(&self) -> Option<(usize, f64, bool)> {
        const OP: &str = "mesh.remove_small_components";
        if self.tool.as_deref() != Some(OP) || !self.targeted_tools().is_empty() {
            return None;
        }
        let spec = mf_spec::builtin::get(OP)?;
        let value = |name: &str| {
            self.op_params
                .get(OP)
                .and_then(|p| p.get(name).cloned())
                .or_else(|| {
                    spec.parameters
                        .iter()
                        .find(|p| p.name == name)
                        .map(|p| p.kind.default_value())
                })
        };
        match (
            value("max_triangles")?,
            value("max_relative_size")?,
            value("keep_largest")?,
        ) {
            (ParamValue::Int(t), ParamValue::Float(r), ParamValue::Bool(k)) => {
                Some((t.max(0) as usize, r, k))
            }
            _ => None,
        }
    }

    /// What Apply on the open "Remove loose pieces" tool would remove from
    /// the active object (MF-ISSUE-052). O(log pieces): asked every frame.
    pub fn removal_preview(&self) -> Option<mf_operations::RemovalPreview> {
        let (max, share, keep) = self.removal_thresholds()?;
        let a = self.current_analysis()?;
        Some(a.defects.pieces.preview(max, share, keep))
    }

    /// Which pieces the open "Remove loose pieces" tool would remove, for
    /// the view's tint.
    fn loose_limit(&self) -> Option<worker::LooseLimit> {
        let (max, share, keep) = self.removal_thresholds()?;
        let pieces = &self.current_analysis()?.defects.pieces;
        let all = pieces.preview(max, share, keep);
        let rest = pieces.preview(max, share, true);
        Some(worker::LooseLimit {
            max_size: pieces.candidate_limit(max, share),
            main: all.pieces > rest.pieces,
        })
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
            (self.defect_shown(s.loose_pieces), FaceStates::LOOSE_PIECE),
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
                loose_limit: None,
                hole_ring: None,
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
        // Intentional openings keep their loops (the mesh didn't change),
        // so they're drawn over the hole outline in their own colour.
        let openings = self
            .active
            .zip(self.active_mesh().cloned())
            .and_then(|(o, mesh)| {
                let ids = self.active_openings();
                (!ids.is_empty()).then_some((o, mesh, OverlayTarget::Loops(ids)))
            });
        for (purpose, want) in [
            (OverlayPurpose::Selection, selection),
            (OverlayPurpose::Hover, hover),
            (OverlayPurpose::Openings, openings),
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
        // Over the hole outline and a marked crack's line: one colour, one
        // meaning (I3).
        if self.defect_shown(self.shown.holes)
            && let Some((object, o)) = ready(OverlayPurpose::Openings)
        {
            layers.push(lines(object, o.origin, &o.lines, theme::OPENING));
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

    /// What View ▸ Metrics shows (`None` until it is first opened).
    pub fn metrics_snapshot(&self) -> Option<&MetricsSnapshot> {
        self.metrics.as_ref()
    }

    /// Refresh the metrics snapshot if the panel is open and it is older
    /// than [`crate::metrics::REFRESH_EVERY`]. Reads cached counters only:
    /// O(kept frames + chunks + components), never O(triangles).
    fn refresh_metrics(&mut self) {
        self.rss.keep(self.show_metrics);
        if !self.show_metrics
            || self
                .metrics
                .as_ref()
                .is_some_and(|m| m.taken.elapsed() < crate::metrics::REFRESH_EVERY)
        {
            return;
        }
        let views = self.object_views();
        let mut gpu = mf_metrics::GpuBufferBytes {
            textures: self.viewport.texture_bytes(),
            ..Default::default()
        };
        for v in &views {
            let b = v.render.gpu.resident_buffers();
            gpu.geometry += b.geometry;
            gpu.state += b.state;
            gpu.lines += b.lines;
        }
        let last_upload_bytes = views
            .iter()
            .find(|v| v.active)
            .map_or(0, |v| v.render.gpu.last_upload().total());
        let mesh = self.active_mesh();
        let mut memory = mf_metrics::MemoryBreakdown {
            mesh_storage_bytes: mesh.map_or(0, Mesh::heap_bytes),
            topology_bytes: mesh.and_then(Mesh::cached_topology).map(|t| t.heap_bytes()),
            scratch_peak_bytes: self.recent_jobs.last().and_then(|j| j.peak_above_start),
            gpu: (gpu.total() > 0).then_some(gpu),
            ..Default::default()
        };
        if let Some(rss) = self.rss.latest() {
            memory.set_process(rss);
        }
        self.metrics = Some(MetricsSnapshot {
            taken: Instant::now(),
            frames: self.frame_times.summary(),
            memory,
            last_upload_bytes,
            geometry_uploaded_bytes: self.geometry_bytes_uploaded(),
            peak_heap_bytes: mf_metrics::is_installed().then(mf_metrics::peak_bytes_since_start),
            jobs: self.recent_jobs.iter().cloned().collect(),
        });
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

/// The browser: holes listed, marked, filled, walked and box-selected
/// (M4 Task 12).
impl MeshFixerApp {
    /// The active object's intentional openings at this revision.
    pub fn active_openings(&self) -> Vec<u32> {
        self.active
            .and_then(|id| self.session.document().object(id))
            .map_or_else(Vec::new, |o| o.openings.clone())
    }

    /// How the browser lists the active revision's holes with its marks,
    /// if that's ready.
    pub fn hole_view(&self) -> Option<&HoleView> {
        self.shared_hole_view().map(Arc::as_ref)
    }

    fn shared_hole_view(&self) -> Option<&Arc<HoleView>> {
        self.hole_view_of(self.active?)
    }

    /// How the browser lists object `id`'s holes at its current revision
    /// and marks, if that's ready: its indices are into that revision's
    /// analysis' `defects.holes`.
    pub fn hole_view_of(&self, id: ObjectId) -> Option<&Arc<HoleView>> {
        let a = self.analysis_of(id)?;
        let object = self.session.document().object(id)?;
        if a.hole_view.openings == object.openings {
            return Some(&a.hole_view);
        }
        let (defects, view) = self.hole_list.as_ref()?;
        (Arc::ptr_eq(defects, &a.defects) && view.openings == object.openings).then_some(view)
    }

    /// Ask for the hole list when the marks changed and the mesh didn't.
    fn refresh_hole_list(&mut self) {
        if self.hole_view().is_some() {
            return;
        }
        let Some(a) = self.current_analysis() else {
            return;
        };
        let (defects, openings) = (a.defects.clone(), self.active_openings());
        let asked = self
            .hole_list_requested
            .as_ref()
            .is_some_and(|(d, o)| Arc::ptr_eq(d, &defects) && *o == openings);
        if !asked {
            self.hole_list_requested = Some((defects.clone(), openings.clone()));
            self.workers.hole_view(defects, openings);
        }
    }

    /// The listed hole at `index` of [`HoleView::listed`].
    fn listed_hole(&self, index: usize) -> Option<Defect> {
        let view = self.hole_view()?;
        let i = *view.listed.get(index)?;
        Some(self.current_analysis()?.defects.holes.get(i)?.clone())
    }

    /// N / Shift+N: select the next (or previous) listed hole after the
    /// selected one, and frame it.
    pub fn next_hole(&mut self, forward: bool) {
        let (Some(object), Some(view)) = (self.active, self.hole_view()) else {
            return;
        };
        let n = view.listed.len();
        if n == 0 {
            return;
        }
        let at = self
            .current_selection()
            .filter(|s| s.kind == SelectionKind::BoundaryLoop && s.ids.len() == 1)
            .and_then(|s| view.position(s.ids[0]));
        let next = match (at, forward) {
            (None, true) => 0,
            (None, false) => n - 1,
            (Some(i), true) => (i + 1) % n,
            (Some(i), false) => (i + n - 1) % n,
        };
        if let Some(d) = self.listed_hole(next) {
            self.reveal = d.selection().map(|(_, id)| id);
            self.tree_find(&TreeItem::Defect(object, d));
        }
    }

    /// The hole whose row the tree should scroll to (once).
    pub fn take_reveal(&mut self) -> Option<u32> {
        self.reveal.take()
    }

    /// Fill these holes (loop ids of the active revision) as one command.
    pub fn fill_loops(&mut self, ids: Vec<u32>) {
        self.fill_loops_up_to(ids, None, None);
    }

    /// As [`Self::fill_loops`], with `max_loop_edges` set to `limit` (the
    /// batch fill's), or else to fit the largest of them.
    /// `label` names the job (the status line, View ▸ Metrics).
    fn fill_loops_up_to(&mut self, ids: Vec<u32>, limit: Option<i64>, label: Option<String>) {
        let (Some(object), Some(a)) = (self.active, self.current_analysis()) else {
            return;
        };
        if ids.is_empty() {
            return;
        }
        let largest = ids
            .iter()
            .filter_map(
                |&id| match a.defects.find(SelectionKind::BoundaryLoop, id) {
                    Some(Defect::Hole { edges, .. }) => Some(*edges as i64),
                    _ => None,
                },
            )
            .max()
            .unwrap_or(3);
        let default = spec_default("mesh.fill_holes", "max_loop_edges");
        let n = ids.len();
        let tool = Tool {
            op_id: "mesh.fill_holes".into(),
            label: label.unwrap_or_else(|| {
                operation_label("mesh.fill_holes", Some((SelectionKind::BoundaryLoop, n)))
                    .expect("fill_holes has plain words")
            }),
            selection: Some(Selection {
                object,
                revision: self.session.current(),
                kind: SelectionKind::BoundaryLoop,
                ids,
            }),
            fixed: BTreeMap::from([(
                "max_loop_edges".into(),
                ParamValue::Int(limit.unwrap_or(largest.max(default))),
            )]),
            kind: ToolKind::OneShot,
            group: None,
        };
        self.run_tool(&tool);
    }

    /// One click on a loose piece's row: remove it.
    pub fn remove_piece(&mut self, component: u32, triangles: usize) {
        let Some(object) = self.active else { return };
        let tool = Tool {
            op_id: "mesh.remove_small_components".into(),
            label: operation_label(
                "mesh.remove_small_components",
                Some((SelectionKind::Component, 1)),
            )
            .expect("remove_small_components has plain words"),
            selection: Some(Selection {
                object,
                revision: self.session.current(),
                kind: SelectionKind::Component,
                ids: vec![component],
            }),
            fixed: BTreeMap::from([
                (
                    "max_triangles".into(),
                    ParamValue::Int(triangles as i64 + 1),
                ),
                ("max_relative_size".into(), ParamValue::Float(100.0)),
                ("keep_largest".into(), ParamValue::Bool(true)),
            ]),
            kind: ToolKind::OneShot,
            group: None,
        };
        self.run_tool(&tool);
    }

    /// The listed holes of `group` that fill_holes can close (loop ids).
    pub fn group_fillable(&self, group: crate::defects::HoleGroup) -> Vec<u32> {
        self.hole_view()
            .map_or_else(Vec::new, |v| v.group_fillable(group).to_vec())
    }

    /// Mark these holes (loop ids of the active revision) as intentional
    /// openings, an undoable step; or unmark them (`mark` false).
    pub fn mark_openings(&mut self, ids: &[u32], mark: bool) {
        let Some(object) = self.active else { return };
        if self.busy.is_some() || ids.is_empty() {
            return;
        }
        let mut openings = self.active_openings();
        if mark {
            openings.extend_from_slice(ids);
        } else {
            openings.retain(|o| !ids.contains(o));
        }
        let n = ids.len();
        let label = match (mark, n) {
            (true, 1) => "Mark intentional opening".to_owned(),
            (true, n) => format!("Mark {n} intentional openings"),
            (false, 1) => "Unmark opening".to_owned(),
            (false, n) => format!("Unmark {n} openings"),
        };
        if self.session.set_openings(object, openings, label) {
            // A marked hole is no longer a target for hole tools.
            if mark {
                self.clear_selection();
            }
            self.after_history_move();
            self.status = Some(if mark {
                format!(
                    "Marked {} as intentional: left out of hole counts and fills",
                    count(n, "opening")
                )
            } else {
                format!("{} listed as holes again", count(n, "opening"))
            });
        }
    }

    /// Hide both side panels, or show both if both are hidden.
    pub fn toggle_side_panels(&mut self) {
        let show = !(self.show_tree || self.show_properties);
        self.show_tree = show;
        self.show_properties = show;
    }

    /// True if the view draws hole markers (they're on by default).
    pub fn markers_shown(&self) -> bool {
        (self.show_hole_markers || self.show_hole_overview) && self.defect_shown(self.shown.holes)
    }

    /// The overview's markers (window points), while it's shown.
    pub fn hole_markers(&self) -> Option<&[egui::Pos2]> {
        if !self.markers_shown() {
            return None;
        }
        self.markers.as_ref().map(|(_, m)| m.positions.as_slice())
    }

    /// Ask for the overview's markers when what they depend on changed.
    fn refresh_markers(&mut self) {
        if !self.markers_shown() {
            self.marker_query = None;
            return;
        }
        let (Some(a), Some(view), Some(camera), Some(rect)) = (
            self.current_analysis(),
            self.shared_hole_view(),
            self.camera,
            self.viewport.last_rect,
        ) else {
            return;
        };
        let query = MarkerQuery {
            camera,
            rect,
            defects: Arc::as_ptr(&a.defects) as usize,
            view: Arc::as_ptr(view) as usize,
        };
        if self.marker_query.is_some_and(|(q, _)| q == query) {
            return;
        }
        let key = self.marker_query.map_or(1, |(_, k)| k + 1);
        // Without every BVH a marker could show for a hidden hole: wait.
        let Some(occluders) = self.occluders() else {
            return;
        };
        let (defects, view) = (a.defects.clone(), view.clone());
        self.marker_query = Some((query, key));
        self.marker_worker.submit(worker::MarkerRequest {
            key,
            defects,
            view,
            camera,
            rect,
            occluders,
            color: theme::HOLE,
        });
    }

    /// Every object's revision, picking BVH and placement: what can hide a
    /// hole. `None` until every object's current revision has its BVH (a
    /// hole can't be called visible before then).
    fn occluders(&self) -> Option<Vec<(Mesh, Arc<Bvh>, glam::DMat4)>> {
        self.session
            .document()
            .objects()
            .iter()
            .filter_map(|o| Some((o, o.mesh()?)))
            .map(|(o, mesh)| {
                let bvh = self
                    .bvhs
                    .get(&o.id)
                    .filter(|(m, _)| Mesh::ptr_eq(m, mesh))
                    .map(|(_, b)| b.clone())?;
                Some((mesh.clone(), bvh, model_matrix(&o.transform)))
            })
            .collect()
    }

    /// A box drawn in the view (Select mode, or B): select the visible
    /// listed holes whose centre is inside it (on a worker).
    fn box_select(&mut self, b: crate::viewport::BoxSelect) {
        let (Some(object), Some(cam), Some(a), Some(view)) = (
            self.active,
            self.camera,
            self.current_analysis(),
            self.shared_hole_view(),
        ) else {
            return;
        };
        let Some(mesh) = self.active_mesh().cloned() else {
            return;
        };
        let Some(occluders) = self.occluders() else {
            self.status = Some("Picking isn't ready yet: try the box again in a moment".into());
            return;
        };
        let (defects, view) = (a.defects.clone(), view.clone());
        self.workers.box_select(
            object, mesh, defects, view, cam, b.size, b.rect, occluders, b.add,
        );
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
        let typing = ctx.memory(|m| m.focused().is_some())
            || egui::Popup::is_any_open(&ctx)
            || self.unit_prompt.is_some()
            || self.confirm.is_some();
        // A file dropped on the window opens, as File ▸ Open does; so does
        // Cmd/Ctrl+O.
        let (dropped, open) = ctx.input_mut(|i| {
            let dropped: Vec<PathBuf> = i
                .raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_owned())
                .collect();
            (
                dropped,
                !typing && i.consume_key(egui::Modifiers::COMMAND, egui::Key::O),
            )
        });
        if self.busy.is_none() {
            if let Some(path) = dropped.first().cloned() {
                if dropped.len() > 1 {
                    self.error = Some(format!(
                        "{} files were dropped; MeshFixer opens one at a time, so only {} was \
                         opened. Drop the others one by one.",
                        dropped.len(),
                        path.display()
                    ));
                }
                self.open(path);
            } else if open {
                self.open_dialog();
            }
        }
        let (next, previous, boxed) = ctx.input_mut(|i| {
            if typing {
                return (false, false, false);
            }
            // Shift+N first: a plain N would match it too.
            let previous = i.consume_key(egui::Modifiers::SHIFT, egui::Key::N);
            let next = i.consume_key(egui::Modifiers::NONE, egui::Key::N);
            (
                next,
                previous,
                i.consume_key(egui::Modifiers::NONE, egui::Key::B),
            )
        });
        if next || previous {
            self.next_hole(next);
        }
        if boxed {
            self.viewport.box_once = true;
        }
        if !typing
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Backslash))
        {
            self.toggle_side_panels();
        }
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
        if escape {
            self.escape();
        }
        if redo {
            self.redo();
        } else if undo {
            self.undo();
        }
        let delete = !typing
            && !ctx.egui_wants_keyboard_input()
            && ctx.input_mut(|i| {
                i.consume_key(egui::Modifiers::NONE, egui::Key::Delete)
                    || i.consume_key(egui::Modifiers::NONE, egui::Key::Backspace)
            });
        if delete {
            self.delete_active_object();
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
        self.unit_dialog(&ctx);
        crate::panels::confirm_window(self, &ctx);
        crate::panels::warnings_window(self, &ctx);
        self.refresh_metrics();
        crate::panels::metrics_window(self, &ctx);
        self.refresh_hole_list();
        self.refresh_overlays();
        self.refresh_states();
        self.refresh_markers();

        let objects = self.object_views();
        let empty = crate::panels::empty_state();
        let placeholder = match self.active_mesh() {
            Some(m) if m.is_empty() => "Empty mesh (0 triangles)",
            Some(_) if self.analysis_failed_for_active() => "Couldn't analyse this object",
            Some(_) => "Analysing…",
            None => empty.as_str(),
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
        let markers = self
            .markers_shown()
            .then(|| self.markers.as_ref().map(|(_, m)| m.mesh.clone()))
            .flatten();
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
                        markers,
                        tool_grabs: None,
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
        if let Some(b) = out.box_select
            && self.busy.is_none()
        {
            self.box_select(b);
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
            || (self.show_hole_overview && self.hole_view().is_none())
            || (self.current_analysis().is_some() && self.hole_view().is_none())
        {
            ctx.request_repaint();
        }
    }
}

/// True if `mesh` is some object's current revision in `doc` (what the
/// view draws).
fn is_current_revision(doc: &mf_command::Document, mesh: &Mesh) -> bool {
    doc.objects()
        .iter()
        .any(|o| o.mesh().is_some_and(|c| Mesh::ptr_eq(c, mesh)))
}

/// An operation's report in plain words for the status line. A 5M fill's
/// raw report lists hundreds of loops: laying that out took ~35 ms of a
/// frame, and it isn't for people anyway.
/// A whole-object tool's name in Properties.
fn tool_label(id: &str) -> String {
    operation_label(id, None).expect("every listed tool has plain words")
}

/// History words for the object operations (they have no Properties
/// button, so no `operation_label`).
fn object_step_label(op_id: &str) -> Option<String> {
    Some(
        match op_id {
            "object.delete" => "Delete object",
            "object.set_transform" => "Move object",
            "object.drop_to_plate" => "Drop to plate",
            _ => return None,
        }
        .into(),
    )
}

fn plain_report(op_id: &str, report: &serde_json::Value) -> String {
    let len = |key: &str| report[key].as_array().map_or(0, Vec::len);
    // allow-swallow: the operation's own report; a count it leaves out reads as 0
    let num = |key: &str| report[key].as_u64().unwrap_or_default();
    let s = |n: u64| if n == 1 { "" } else { "s" };
    match op_id {
        "object.delete" => "Object deleted. Undo brings it back.".into(),
        "mesh.fill_holes" => {
            let (filled, skipped) = (len("filled_loops") as u64, len("skipped_loops") as u64);
            let faces = num("created_faces");
            let mut text = format!(
                "Filled {} hole{} with {} new triangle{}",
                thousands(filled),
                s(filled),
                thousands(faces),
                s(faces)
            );
            if skipped > 0 {
                text.push_str(&format!(
                    "; {} left open: {}",
                    thousands(skipped),
                    skip_reasons(&report["skipped_loops"])
                ));
            }
            // MF-ISSUE-055: curvature matching on large holes, in the
            // operation's own words.
            for w in report["warnings"].as_array().into_iter().flatten() {
                if let Some(w) = w.as_str() {
                    text.push_str(". ");
                    text.push_str(w);
                }
            }
            text
        }
        "mesh.remove_small_components" => {
            let (pieces, tris) = (num("removed_components"), num("removed_triangles"));
            format!(
                "Removed {} piece{} ({} triangle{})",
                thousands(pieces),
                s(pieces),
                thousands(tris),
                s(tris)
            )
        }
        "mesh.compact" => {
            let (faces, vertices) = (num("reclaimed_faces"), num("reclaimed_vertices"));
            format!(
                "Reclaimed unused space: {faces} triangle slot{} and {vertices} vertex slot{}",
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

/// Why a batch fill left holes open, grouped by reason in plain words:
/// "2 pinched (pass through one point twice), 1 over this fill method's
/// 1,000-edge limit" (M4 final UX review I7).
fn skip_reasons(skipped: &serde_json::Value) -> String {
    let mut groups: Vec<(&str, u64)> = Vec::new();
    for l in skipped.as_array().into_iter().flatten() {
        let why = match l["reason"].as_str() {
            Some("too_large") => "over this fill method's 1,000-edge limit",
            Some("self_touching") => "pinched (pass through one point twice)",
            Some("fairing_did_not_converge") => "couldn't be shaped to the surface",
            Some("chord_used_by_another_hole") => {
                "blocked by another hole's fill (fill them one at a time)"
            }
            Some("untriangulable") => "flat or folded onto the mesh",
            _ => "couldn't be filled",
        };
        match groups.iter_mut().find(|(w, _)| *w == why) {
            Some((_, n)) => *n += 1,
            None => groups.push((why, 1)),
        }
    }
    groups
        .iter()
        .map(|(why, n)| format!("{} {why}", thousands(*n)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Why an import failed, and what to do about it.
fn import_failed(e: &IoError) -> String {
    match e {
        IoError::LimitExceeded {
            what: "declared triangle count",
            limit,
            ..
        } => format!(
            "This file is larger than MeshFixer currently opens ({} M triangles). Split it in \
             another tool, or ask us to raise the limit.",
            limit / 1_000_000
        ),
        IoError::LimitExceeded { what, limit, found } => format!(
            "This file is larger than MeshFixer currently opens (its {what} is {found}, over \
             the limit of {limit}). Split it in another tool, or ask us to raise the limit."
        ),
        e => format!(
            "Import failed: {e}. Check that the file opens in another program, or export it \
             again from the program that made it."
        ),
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

/// The status line when a history move clears the selection: a filled
/// hole says so with its face count; anything else says why
/// (MF-ISSUE-036: a hole removed with its surroundings is not "filled").
fn selection_cleared(e: &TranslateError) -> String {
    match e {
        TranslateError::LoopClosed { faces } => {
            let plural = if faces.len() == 1 { "" } else { "s" };
            format!(
                "Hole filled ({} new triangle{plural}); selection cleared",
                faces.len()
            )
        }
        other => selection_lost(other),
    }
}

/// Why a selection couldn't follow a history move, in plain words.
fn selection_lost(e: &TranslateError) -> String {
    let why = match e {
        TranslateError::NoPath => "that step is on another branch",
        TranslateError::ObjectMissing => "its object doesn't exist at this step",
        TranslateError::AllLost { .. } => "what was selected doesn't exist at this step",
        TranslateError::LoopClosed { .. } => "the hole is closed at this step",
        TranslateError::LoopRemoved => "the hole was removed along with the faces around it",
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
            operation_label(&rec.op_id, target).or_else(|| object_step_label(&rec.op_id))
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
            "Filled 2 holes with 10 new triangles; 1 left open: 1 couldn't be filled"
        );
        // MF-ISSUE-055: the curvature-limit warning reaches the status line.
        let warned = serde_json::json!({
            "created_faces": 10,
            "filled_loops": [{"id": 1}],
            "skipped_loops": [],
            "warnings": [mf_operations::curvature_limit_warning(1)],
        });
        assert_eq!(
            plain_report("mesh.fill_holes", &warned),
            format!(
                "Filled 1 hole with 10 new triangles. {}",
                mf_operations::curvature_limit_warning(1)
            )
        );
        // Skips are grouped by reason, in plain words (I7).
        let skipped = serde_json::json!({
            "created_faces": 1200,
            "filled_loops": [{"id": 1}],
            "skipped_loops": [
                {"id": 2, "reason": "self_touching"},
                {"id": 3, "reason": "too_large"},
                {"id": 4, "reason": "self_touching"},
            ],
        });
        assert_eq!(
            plain_report("mesh.fill_holes", &skipped),
            "Filled 1 hole with 1,200 new triangles; 3 left open: 2 pinched (pass through one \
             point twice), 1 over this fill method's 1,000-edge limit"
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

    /// A file over the import limit says so in plain words, with what to
    /// do next (Task 12 copy pass).
    #[test]
    fn import_errors_say_what_to_do_next() {
        let big = IoError::LimitExceeded {
            what: "declared triangle count",
            limit: 50_000_000,
            found: 80_000_000,
        };
        assert_eq!(
            import_failed(&big),
            "This file is larger than MeshFixer currently opens (50 M triangles). Split it in \
             another tool, or ask us to raise the limit."
        );
        let other = IoError::Parse {
            line: 3,
            message: "bad vertex".into(),
        };
        assert!(import_failed(&other).ends_with("from the program that made it."));
    }

    /// MF-ISSUE-036: a hole that was filled and a hole whose surroundings
    /// were removed clear the selection with different messages.
    #[test]
    fn a_filled_hole_and_a_removed_hole_say_which() {
        let closed = selection_cleared(&TranslateError::LoopClosed {
            faces: vec![mf_geometry::FaceId(1), mf_geometry::FaceId(2)],
        });
        assert_eq!(closed, "Hole filled (2 new triangles); selection cleared");
        let removed = selection_cleared(&TranslateError::LoopRemoved);
        assert_eq!(
            removed,
            "Selection cleared: the hole was removed along with the faces around it"
        );
        assert!(!removed.contains("filled"), "{removed}");
    }

    /// The status line explains a lost selection in words, never a debug dump.
    #[test]
    fn lost_selection_is_explained_in_plain_words() {
        for e in [
            TranslateError::NoPath,
            TranslateError::ObjectMissing,
            TranslateError::AllLost { lost: vec![1] },
            TranslateError::LoopClosed { faces: vec![] },
            TranslateError::LoopRemoved,
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
