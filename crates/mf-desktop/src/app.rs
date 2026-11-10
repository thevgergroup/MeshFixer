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
    Bvh, CameraMove, FaceStates, Gizmo, GizmoMode, Handle, OrbitCamera, PlateSnap, SelectionCue,
    Snap, clip_radius, model_matrix, screen_position,
};
use mf_runtime::RunContext;
use mf_spec::ParamValue;

use crate::analysis::{AnalysisCache, LatestWorker};
use crate::defects::{Defect, HoleNumbers, HoleView, count, operation_label, thousands};
use crate::import_dialog::UnitPrompt;
use crate::metrics::{JobStart, MetricsSnapshot, RecentJobs, Rss};
use crate::tools::{Interactive, LegendRow, ToolGroup, ToolKind};
use crate::transform_tool::{self, Dragging, Field, Placement, TransformTool};
use crate::viewport::{
    Click, NavAction, ObjectView, ToolPointer, ViewLayer, ViewOptions, Viewport, ZOOM_STEP,
};
use crate::worker::{
    self, Analysis, Message, Overlay, OverlayPurpose, OverlayTarget, PickTarget, PoolSizes,
    RenderMesh, StateInputs, ViewHover, Workers,
};
use crate::{t, theme};
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

/// On a laptop-size window (narrower than [`crate::tool_panels::NARROW_WINDOW`])
/// the tool panel and the Browser can't both be open (ui-plan.md §3):
/// which one the user last asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NarrowFocus {
    Tool,
    Browser,
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
    analysis_failed: Option<Mesh>,
    /// Measures an analysed revision again after a transform-only change
    /// (a move, a turn, an undo of one): see `worker::remeasure`.
    remeasurer: LatestWorker<worker::RemeasureRequest>,
    /// The (revision, transform) last sent to it.
    remeasure_asked: Option<(Mesh, [f64; 12])>,
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
    /// The one-shot operation open inside the Repair tool (its catalogue
    /// id, `tools::CATALOGUE`): its settings replace Repair's list.
    pub tool: Option<String>,
    /// The interactive tool's state while it is open (Transform's gizmo
    /// mode, drag and fields; `tools::Interactive`).
    pub interactive: Option<Interactive>,
    /// The shell's registries (ui-plan.md §5): tools, commands, panels,
    /// overlays, inspector sections, context actions, settings pages.
    pub shell: Arc<crate::shell::Shell>,
    /// The tool open on the rail (a `shell::Tool` id): "select" when no
    /// tool is open.
    pub active_tool: &'static str,
    /// The Browser: `None` until the user opens or closes it (then it
    /// follows the default: open unless the document is one object with
    /// nothing to fix).
    pub browser_open: Option<bool>,
    /// The right dock's tab (a `shell::Panel` id).
    pub right_tab: &'static str,
    /// On a laptop-size window the tool panel and the Browser can't both
    /// be open: which one the user last asked for.
    pub narrow_focus: NarrowFocus,
    /// View ▸ Overlays: the popover listing what each colour means.
    pub show_overlays: bool,
    /// The Settings window.
    pub show_settings: bool,
    /// The first-use hint ("Pick a tool on the left…"): shown with a
    /// model open until a tool is used or it is dismissed.
    pub first_use_hint: bool,
    /// How the model is shaded (View > Clay / Studio / X-ray, Wireframe).
    pub shading: mf_render::Shading,
    /// The Shape tools' state (UI plan step 5): Split's plane, Combine's
    /// tool and operation, Make solid's settings.
    pub split: crate::shape_tools::SplitState,
    pub combine: crate::shape_tools::CombineState,
    /// Objects selected together (Shift-click in the view or the
    /// Browser), in the order they were picked; the active object is one
    /// of them. Empty (or one) means just the active object. Combine and
    /// Split act on all of them; H toggles their roles.
    pub picked: Vec<ObjectId>,
    pub solid: crate::shape_tools::SolidState,
    /// The last operation refusal, shown in its tool's panel in plain
    /// words with the next action.
    pub refusal: Option<crate::shape_tools::Refusal>,
    /// The UI's context, for background work that asks for a frame.
    egui_ctx: Option<egui::Context>,
    /// A placement committed from the Transform tool, drawn until its
    /// command finishes (so the object doesn't jump back meanwhile).
    preview: Option<(ObjectId, [f64; 12])>,
    /// What the status line says once that command commits.
    pending_status: Option<String>,
    /// The lowest world z of an object at a transform (the plate snap),
    /// found on a worker; and what was last asked for.
    lowest: Option<(ObjectId, Mesh, [f64; 12], Option<f64>)>,
    lowest_asked: Option<(ObjectId, Mesh, [f64; 12])>,
    lowest_worker: LatestWorker<(ObjectId, Mesh, [f64; 12])>,
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
    /// The side panels (View > Hide side panels, Cmd/Ctrl+\): hidden, the
    /// view has the window but the rail (M4 final UX I5).
    pub side_panels_hidden: bool,
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
        let remeasurer = {
            let (tx, ctx) = (tx.clone(), cc.egui_ctx.clone());
            LatestWorker::new(workers.background(), worker::remeasure, move |msg| {
                // allow-swallow: the receiver only disappears while the app is closing
                let _ = tx.send(msg);
                ctx.request_repaint();
            })
        };
        let lowest_worker = {
            let (tx, ctx) = (tx.clone(), cc.egui_ctx.clone());
            LatestWorker::new(
                workers.background(),
                |(object, mesh, transform): (ObjectId, Mesh, [f64; 12]), run: &RunContext| {
                    let z = mf_operations::world_min_z_with(&mesh, &transform, run)?;
                    Ok(Message::LowestPoint {
                        object,
                        mesh,
                        transform,
                        z,
                    })
                },
                move |msg| {
                    // allow-swallow: the receiver only disappears while the app is closing
                    let _ = tx.send(msg);
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
            remeasurer,
            remeasure_asked: None,
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
            shell: Arc::new(crate::shell::builtin()),
            active_tool: crate::tool_panels::SELECT,
            browser_open: None,
            right_tab: crate::panels::BROWSER,
            narrow_focus: NarrowFocus::Tool,
            show_overlays: false,
            show_settings: false,
            first_use_hint: true,
            shading: mf_render::Shading::default(),
            split: Default::default(),
            combine: Default::default(),
            picked: Vec::new(),
            solid: Default::default(),
            refusal: None,
            egui_ctx: Some(cc.egui_ctx.clone()),
            preview: None,
            pending_status: None,
            lowest: None,
            lowest_asked: None,
            lowest_worker,
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
            side_panels_hidden: false,
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
                    metadata: o.metadata(),
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

    /// Undo (mid-drag, it cancels the drag instead, as Esc does).
    pub fn undo(&mut self) {
        if self.cancel_tool_drag() {
            return;
        }
        if self.busy.is_none() {
            self.session.undo();
            self.after_history_move();
        }
    }

    /// Redo (mid-drag, it cancels the drag instead).
    pub fn redo(&mut self) {
        if self.cancel_tool_drag() {
            return;
        }
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
        // A tool that hides the defects hides what hovering would find.
        let pointer = pointer.filter(|_| !self.tool_hides_defects());
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

    /// Esc, in order (ui-plan.md §5): stop the running job; else cancel
    /// the open tool's drag; else go one level back inside the tool (a
    /// sub-tool's settings, a selection); else leave the tool; else clear
    /// the selection. One step per press.
    pub fn escape(&mut self) {
        if self.can_cancel() {
            self.cancel_job();
            return;
        }
        if self.cancel_tool_drag() {
            return;
        }
        let tool = self.active_tool_def();
        if (tool.back)(self) {
            return;
        }
        if self.active_tool != crate::tool_panels::SELECT {
            self.close_rail_tool();
            return;
        }
        self.clear_selection();
    }

    /// The open tool's registration.
    pub fn active_tool_def(&self) -> crate::shell::Tool {
        *self
            .shell
            .tool(self.active_tool)
            .or_else(|| self.shell.tool(crate::tool_panels::SELECT))
            .expect("Select is registered")
    }

    /// Open rail tool `id` (a `shell::Tool` id): the open one closes,
    /// keeping its work. Opening the open one does nothing; an
    /// unavailable one stays shut (its reason in the status line).
    pub fn open_rail_tool(&mut self, id: &str) {
        let Some(tool) = self.shell.tool(id).copied() else {
            return;
        };
        if let crate::shell::Availability::Unavailable(why) = (tool.availability)(self) {
            self.status = Some(why);
            return;
        }
        if tool.id != crate::tool_panels::SELECT {
            self.first_use_hint = false;
            self.narrow_focus = NarrowFocus::Tool;
        }
        if self.active_tool == tool.id {
            // The open tool's rail button takes it back to its top level.
            while (tool.back)(self) {}
            return;
        }
        let old = self.active_tool_def();
        (old.on_close)(self, crate::shell::Close::Commit);
        self.active_tool = tool.id;
        (tool.on_open)(self);
    }

    /// The UI's context (background work asks it for a frame).
    pub fn repaint_context(&self) -> Option<egui::Context> {
        self.egui_ctx.clone()
    }

    /// Prepare and start `inv` on the job pool (progress and Cancel in the
    /// status bar), as a whole-object tool's Apply does.
    pub fn run_invocation(&mut self, inv: Invocation, label: String) {
        match self.session.prepare(inv) {
            Ok(prepared) => {
                let run = self.start_job(format!("{label}…"), true);
                self.workers.run(prepared, run);
            }
            Err(e) => {
                self.error = Some(format!(
                    "{label} can't run: {e}. Change the settings and Apply again."
                ))
            }
        }
    }

    /// The gizmo the open tool draws (Transform's, Split's plane), with
    /// its highlighted handle.
    pub fn tool_gizmo(&self) -> Option<(Gizmo, Option<Handle>)> {
        self.transform_gizmo()
            .map(|g| (g, self.gizmo_highlight()))
            .or_else(|| crate::shape_tools::split_gizmo(self))
    }

    /// Run a registered command.
    pub fn run_command(&mut self, command: &crate::shell::Command) {
        if !(command.enabled)(self) {
            return;
        }
        match &command.action {
            crate::shell::Action::Run(run) => run(self),
            crate::shell::Action::Operation(op) => {
                if let Some(tool) = op.strip_prefix("tool:") {
                    if tool == crate::tool_panels::TRANSFORM {
                        self.open_transform();
                    } else {
                        self.open_rail_tool(tool);
                    }
                    return;
                }
                match op.as_str() {
                    "object.set_transform" => self.open_transform(),
                    "object.drop_to_plate" => self.drop_to_plate(),
                    "object.delete" => self.delete_active_object(),
                    "mesh.inspect" => self.open_rail_tool(crate::tool_panels::INSPECT),
                    other if crate::tools::entry(other).is_some() => self.open_tool(other),
                    _ => {
                        self.status = Some(format!(
                            "{} isn't offered in the app yet",
                            command.title.text()
                        ));
                    }
                }
            }
        }
    }

    /// Close the open rail tool: back to Select.
    pub fn close_rail_tool(&mut self) {
        self.open_rail_tool(crate::tool_panels::SELECT);
    }

    /// Cancel the open interactive tool's drag, if one is under way:
    /// nothing is committed. Returns whether there was one.
    pub fn cancel_tool_drag(&mut self) -> bool {
        match self.interactive.as_mut() {
            None => false,
            Some(Interactive::Transform(t)) => t.dragging.take().is_some(),
        }
    }

    /// Open a whole-object tool by its catalogue id: Transform opens on
    /// the rail; a one-shot operation opens its settings inside Repair.
    pub fn open_tool(&mut self, id: &str) {
        let interactive = crate::tools::entry(id).is_some_and(|e| e.kind == ToolKind::Interactive);
        if interactive {
            self.open_rail_tool(crate::tool_panels::TRANSFORM);
            return;
        }
        if self.active_tool != crate::tool_panels::REPAIR {
            self.open_rail_tool(crate::tool_panels::REPAIR);
        }
        if self.active_tool == crate::tool_panels::REPAIR {
            self.tool = Some(id.to_owned());
        }
    }

    /// Close the one-shot operation's settings (back to Repair's list), or
    /// the Transform tool.
    pub fn close_tool(&mut self) {
        if self.active_tool == crate::tool_panels::TRANSFORM {
            self.close_rail_tool();
        }
        self.tool = None;
    }

    /// Leave the Transform tool (its state goes; the rail goes back to
    /// Select).
    pub fn leave_interactive(&mut self) {
        self.interactive = None;
        if self.active_tool == crate::tool_panels::TRANSFORM {
            self.active_tool = crate::tool_panels::SELECT;
        }
    }

    /// What the Overlays popover lists: the open tool's own rows, or every
    /// registered overlay's legend entry.
    pub fn legend_rows(&self) -> Vec<LegendRow> {
        let tool = self.active_tool_def();
        (tool.overlays)(self).unwrap_or_else(|| {
            self.shell
                .overlays()
                .iter()
                .map(|o| o.legend.clone())
                .collect()
        })
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
        // Another object, not one of those picked together, ends the
        // multi-selection.
        if !self.picked.contains(&id) {
            self.picked.clear();
        }
        if self.active != Some(id) && self.selection.as_ref().is_some_and(|s| s.object != id) {
            self.selection = None;
            self.status = Some("Selection cleared (another object is active)".into());
        }
        if self.active != Some(id) {
            self.inspected = None;
        }
        self.active = Some(id);
    }

    /// The objects selected together, in pick order: those Shift-clicked
    /// (that still exist), or just the active object.
    pub fn selected_objects(&self) -> Vec<ObjectId> {
        let doc = self.session.document();
        let picked: Vec<ObjectId> = self
            .picked
            .iter()
            .copied()
            .filter(|&id| doc.object(id).is_some())
            .collect();
        if picked.len() > 1 {
            picked
        } else {
            self.active.into_iter().collect()
        }
    }

    /// Shift-click on an object (in the view or the Browser): add it to
    /// the objects selected together (it becomes active), or take it out
    /// (the last one picked becomes active).
    pub fn toggle_picked(&mut self, id: ObjectId) {
        if self.picked.is_empty()
            && let Some(active) = self.active
        {
            self.picked.push(active);
        }
        if let Some(k) = self.picked.iter().position(|&p| p == id) {
            if self.picked.len() == 1 {
                return;
            }
            self.picked.remove(k);
            if self.active == Some(id)
                && let Some(&last) = self.picked.last()
            {
                self.set_active(last);
            }
        } else {
            self.picked.push(id);
            self.set_active(id);
        }
        self.clear_selection();
    }

    /// True if `id`'s role is Hole.
    pub fn is_hole(&self, id: ObjectId) -> bool {
        self.session
            .document()
            .object(id)
            .is_some_and(|o| o.is_hole())
    }

    /// Toggle the Solid/Hole role of `ids` (H, or the Browser's menu) as
    /// one undoable step: all become solids if every one is a hole,
    /// otherwise all become holes.
    pub fn toggle_role(&mut self, ids: &[ObjectId]) {
        if self.busy.is_some() || ids.is_empty() {
            return;
        }
        let to_hole = !ids.iter().all(|&id| self.is_hole(id));
        let changes = ids
            .iter()
            .map(|&object| mf_command::PropertyChange {
                object,
                key: mf_command::ROLE.into(),
                value: to_hole.then(|| mf_command::HOLE.to_owned()),
            })
            .collect();
        let label = match (to_hole, ids.len()) {
            (true, 1) => t!("role.make_hole"),
            (false, 1) => t!("role.make_solid"),
            (true, n) => t!("role.make_holes", n = n),
            (false, n) => t!("role.make_solids", n = n),
        };
        if self.session.set_properties(changes, label.clone()) {
            self.after_history_move();
            self.status = Some(label);
        }
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
            // A command that creates an object (Split's two parts, a
            // boolean's result) can take the document from one object to
            // several. The Browser's visibility is decided once and kept
            // sticky after that (so repairing everything doesn't fold it
            // away); on a single-object document that decision is often
            // "closed", which would hide the very parts the owner needs
            // to pick between to target one of them. Reopen it whenever a
            // created object means there's now more than one (matching
            // `browser_default`'s own rule), rather than leaving the
            // owner to guess that it's there but closed.
            if self.session.document().objects().len() > 1 {
                self.browser_open = Some(true);
            }
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
                // The Transform tool's placement is the document's now (or
                // wasn't committed): stop drawing it over the document's.
                self.preview = None;
                let placed = self.pending_status.take();
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
                        self.status = Some(match placed {
                            Some(text) if !too_late => text,
                            _ => self.object_report(op, &r).unwrap_or_else(|| report(&r)),
                        });
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
                    // The Shape tools' refusals show in their panel, in
                    // plain words with the next action.
                    Err(CommandError::Operation(mf_operations::OpError::Failed {
                        code,
                        message,
                    })) if crate::shape_tools::op_of(self.active_tool).is_some() => {
                        self.status = Some(format!("Nothing was changed: {message}"));
                        self.refusal = Some(crate::shape_tools::Refusal {
                            op_id: crate::shape_tools::op_of(self.active_tool)
                                .expect("checked above")
                                .to_owned(),
                            code: code.to_owned(),
                            message,
                        });
                    }
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
                // Shift-click on an object (a whole-object pick: with a
                // whole-object tool open, or on its main body with nothing
                // smaller selected) adds it to, or takes it out of, the
                // objects selected together.
                if is_current
                    && add
                    && (self.tool_hides_defects()
                        || (self.current_selection().is_none()
                            && self.current_inspected().is_none()
                            && matches!(&picked, Some((SelectionKind::Component, ids)) if ids == &[0])))
                    && picked.is_some()
                {
                    self.toggle_picked(object);
                    return;
                }
                if is_current && picked.is_some() && !add {
                    // A plain click on an object selects just it.
                    self.picked.clear();
                }
                if is_current {
                    // With Transform open, or on the main body with no
                    // defect targeted, a click makes the object the target
                    // (it gets the whole-object tools, Transform first).
                    let whole = self.tool_hides_defects()
                        || (!add
                            && self.current_selection().is_none()
                            && self.current_inspected().is_none()
                            && matches!(&picked, Some((SelectionKind::Component, ids)) if ids == &[0]));
                    match picked {
                        Some(_) if whole => {
                            self.set_active(object);
                            self.clear_selection();
                        }
                        Some((kind, ids)) => {
                            // A hit on another object makes it active
                            // (MF-ISSUE-042); `set_active` clears a
                            // selection that belongs to a different object.
                            self.set_active(object);
                            self.select(object, kind, ids, add);
                        }
                        None if self.tool_hides_defects() => {}
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
            Message::AnalysisFailed { mesh, message, .. } => {
                if self
                    .requested
                    .as_ref()
                    .is_some_and(|(m, _)| Mesh::ptr_eq(m, &mesh))
                {
                    self.requested = None;
                }
                self.error = Some(format!(
                    "Couldn't analyse this object, so its defects can't be shown \
                     (internal error: {message}). Undo the last step, or open the file again."
                ));
                self.analysis_failed = Some(mesh);
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
            Message::Remeasured {
                mesh,
                transform,
                defects,
                hole_view,
            } => {
                // Only for the placement the object has now (a later move
                // asks again).
                let current = self.session.document().objects().iter().any(|o| {
                    o.transform == transform && o.mesh().is_some_and(|m| Mesh::ptr_eq(m, &mesh))
                });
                if current {
                    // A use of the revision shown: it becomes the most
                    // recently used (fix round 2, F3).
                    let mut old = None;
                    self.analyses.update(&mesh, |a| {
                        a.transform = transform;
                        old = Some((
                            std::mem::replace(&mut a.defects, defects),
                            std::mem::replace(&mut a.hole_view, hole_view),
                        ));
                    });
                    if let Some(old) = old {
                        self.workers.discard(old);
                    }
                }
                self.workers.discard(mesh);
            }
            Message::LowestPoint {
                object,
                mesh,
                transform,
                z,
            } => {
                if let Some(old) = self.lowest.replace((object, mesh, transform, z)) {
                    self.workers.discard(old);
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
                    .is_some_and(|(m, _)| Mesh::ptr_eq(m, &mesh))
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
        self.analysis_for(object.mesh()?)
    }

    /// The analysis of `mesh`, if it's ready. Its topology, render mesh
    /// and overlays belong to the mesh alone; its measurements (perimeters,
    /// sizes, where each defect is) were made at `Analysis::transform`,
    /// which lags the object's for a moment after a move or turn while
    /// they're made again ([`Self::refresh_analysis`]): the previous values
    /// show meanwhile, and nothing goes blank.
    fn analysis_for(&self, mesh: &Mesh) -> Option<&Analysis> {
        self.analyses.get(mesh)
    }

    /// True while the active object's measurements are being made again
    /// after a transform-only change.
    pub fn is_remeasuring(&self) -> bool {
        let Some(o) = self
            .active
            .and_then(|id| self.session.document().object(id))
        else {
            return false;
        };
        o.mesh()
            .and_then(|m| self.analysis_for(m))
            .is_some_and(|a| a.transform != o.transform)
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
        } else if let Some((render, measured)) = self
            .analysis_for(&mesh)
            .map(|a| (a.render.clone(), a.transform))
        {
            if measured != transform {
                self.remeasure(&mesh, transform);
            }
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
            .is_some_and(|(m, _)| Mesh::ptr_eq(m, &mesh))
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

    /// Measure the active revision `mesh` again at `transform` (on a
    /// worker), once per (revision, transform).
    fn remeasure(&mut self, mesh: &Mesh, transform: [f64; 12]) {
        if self
            .remeasure_asked
            .as_ref()
            .is_some_and(|(m, t)| Mesh::ptr_eq(m, mesh) && *t == transform)
        {
            return;
        }
        let Some((a, object)) = self.analyses.get(mesh).zip(self.active) else {
            return;
        };
        // allow-swallow: an object with no numbering yet numbers its holes from 1
        let numbers = self.hole_numbers.get(&object).cloned().unwrap_or_default();
        let request = worker::RemeasureRequest {
            mesh: mesh.clone(),
            diagnostics: a.diagnostics.clone(),
            previous: a.defects.clone(),
            from: a.transform,
            to: transform,
            numbers,
            openings: self.active_openings(),
        };
        if let Some(old) = self.remeasure_asked.replace((mesh.clone(), transform)) {
            self.workers.discard(old);
        }
        self.remeasurer.submit(request);
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
                    transform: self.shown_transform(o.id).unwrap_or(o.transform),
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
                    picked: self.picked.len() > 1 && self.picked.contains(&o.id),
                    hole: o.is_hole(),
                })
            })
            .collect()
    }

    /// An object's bounds (object space), from vertices built on a worker:
    /// `None` until they're ready, `Some(None)` for an empty mesh.
    fn object_bounds(&self, o: &mf_command::Object) -> Option<Option<Aabb>> {
        let mesh = o.mesh()?;
        let vertices = if Some(o.id) == self.active {
            // Bounds are in object space: any analysis of this revision
            // has them, whatever transform it measured at (after a move,
            // the new one is still being analysed).
            &self.analyses.get(mesh)?.render
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

    /// A row of the tree Shift-clicked: an object row adds its object to
    /// (or takes it out of) the objects selected together; any other row
    /// is a plain click.
    pub fn tree_select_add(&mut self, item: &TreeItem) {
        match item {
            TreeItem::Object(id) => self.toggle_picked(*id),
            other => self.tree_select(other),
        }
    }

    /// A row of the tree clicked: select its item (the camera stays).
    pub fn tree_select(&mut self, item: &TreeItem) {
        match item {
            TreeItem::Object(id) => {
                if self.active != Some(*id) {
                    self.tree_hold = Some((self.active, self.now + TREE_HOLD_SECONDS));
                }
                // A plain click selects just this object.
                self.picked.clear();
                self.set_active(*id);
                // The object's own row is a whole-object pick (like
                // "Model"): any sub-selection left over, on this object or
                // another, would silently block Delete and whole-object
                // tools, with nothing in the row to say why.
                self.clear_selection();
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
        self.analysis_for(object.mesh()?)
    }

    /// True if a row's item is the current target (the row shows selected).
    pub fn tree_selected(&self, item: &TreeItem) -> bool {
        match item {
            // An object row says which object is active by its text; the
            // objects selected together are highlighted.
            TreeItem::Object(id) => self.picked.len() > 1 && self.picked.contains(id),
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
            hole_ring: (self.show_hole_overview && !self.tool_hides_defects())
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
        self.show_boundaries && on && !self.tool_hides_defects()
    }

    /// True while an open tool hides the defect and recent-repair
    /// overlays (Transform: D8, so the axis colours mean one thing).
    pub fn tool_hides_defects(&self) -> bool {
        let tool = self.active_tool_def();
        (tool.hides_defects)(self)
    }

    /// True if the view outlines the holes this frame.
    pub fn holes_drawn(&self) -> bool {
        self.defect_shown(self.shown.holes)
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
            (
                s.recent_repairs && !self.tool_hides_defects(),
                FaceStates::RECENT_REPAIR,
            ),
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
            // A tool that hides the defects hides their hover outline too.
            .filter(|(_, t)| !self.tool_hides_defects() || *t == OverlayTarget::Object)
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
            (Some(m), Some(mesh)) => Mesh::ptr_eq(m, mesh),
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

    /// Hide both side panels (the tool panel and the Browser), or show
    /// them again.
    pub fn toggle_side_panels(&mut self) {
        self.side_panels_hidden = !self.side_panels_hidden;
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

/// The Transform tool (MF-OBJ-001, Phase 1 Task 5): its gizmo, its keys
/// and its commands. Placement arithmetic is in `transform_tool.rs`.
impl MeshFixerApp {
    /// The open Transform tool, if it is.
    pub fn transform_tool(&self) -> Option<&TransformTool> {
        match &self.interactive {
            Some(Interactive::Transform(t)) => Some(t),
            None => None,
        }
    }

    pub fn transform_tool_mut(&mut self) -> Option<&mut TransformTool> {
        match &mut self.interactive {
            Some(Interactive::Transform(t)) => Some(t),
            None => None,
        }
    }

    /// Open the Transform tool on the active object (T): the whole object
    /// becomes the target.
    pub fn open_transform(&mut self) {
        if self.active_mesh().is_none() {
            return;
        }
        self.clear_selection();
        if self.transform_tool().is_none() {
            self.open_rail_tool(crate::tool_panels::TRANSFORM);
        }
    }

    /// Transform's own state, when it opens (its rail `on_open`).
    pub(crate) fn start_transform(&mut self) {
        self.interactive = crate::tools::Interactive::open(self, transform_tool::ID);
        if self.interactive.is_none() {
            self.active_tool = crate::tool_panels::SELECT;
        }
    }

    /// The tools' keys, before the app's (I8): each catalogue tool's
    /// shortcut opens it (T: Transform), and the open interactive tool
    /// gets its own keys (Transform: W, E and R pick Move, Rotate and
    /// Scale, as in Meshmixer). A key no open tool uses is left for the
    /// rest of the app.
    fn tool_keys(&mut self, ctx: &egui::Context) {
        // The open tool's own keys first: they win over another tool's
        // shortcut while it is open.
        let tool = self.active_tool_def();
        let keys = (tool.keys)(self);
        let pressed = ctx.input_mut(|i| {
            keys.iter()
                .find(|(k, _)| i.consume_key(egui::Modifiers::NONE, *k))
                .map(|(_, what)| *what)
        });
        if let Some(k) = pressed {
            (tool.on_key)(self, k);
        }
        let shortcuts: Vec<(&'static str, egui::Key)> = self
            .shell
            .tools()
            .iter()
            .filter_map(|t| t.shortcut.map(|k| (t.id, k)))
            .collect();
        for (id, key) in shortcuts {
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, key)) {
                if id == crate::tool_panels::TRANSFORM {
                    self.open_transform();
                } else {
                    self.open_rail_tool(id);
                }
            }
        }
    }

    /// Switch the gizmo's mode. A sheared transform only moves: its
    /// rotation and scale can't be told apart from the shear.
    pub fn set_transform_mode(&mut self, mode: GizmoMode) {
        let sheared = self
            .active_placement()
            .is_some_and(|p| p.rotation_deg.is_none());
        if let Some(t) = self.transform_tool_mut() {
            if sheared && mode != GizmoMode::Move {
                self.status = Some(format!(
                    "{}: only Move works on it. Rotate and Scale would mix with the shear.",
                    transform_tool::SHEARED_NOTE
                ));
                return;
            }
            t.mode = mode;
            t.dragging = None;
        }
    }

    /// Transform works on a whole object: a defect targeted (from the
    /// tree) or the object gone closes it.
    fn keep_transform_target(&mut self) {
        if self.transform_tool().is_some()
            && (self.active_mesh().is_none()
                || self.current_selection().is_some()
                || self.current_inspected().is_some())
        {
            self.leave_interactive();
        }
    }

    /// The bounding-box centre of object `id` in object space, once its
    /// bounds are known.
    fn object_centre(&self, id: ObjectId) -> Option<glam::DVec3> {
        let o = self.session.document().object(id)?;
        let bb = self.object_bounds(o)??;
        Some(glam::DVec3::from(bb.center()))
    }

    /// The transform object `id` is drawn at: a drag's, then a committed
    /// placement still being applied, else `None` (the document's).
    pub fn shown_transform(&self, id: ObjectId) -> Option<[f64; 12]> {
        if let Some(d) = self.transform_tool().and_then(|t| t.dragging)
            && d.object == id
        {
            return Some(d.current);
        }
        self.preview.filter(|(o, _)| *o == id).map(|(_, t)| t)
    }

    /// The active object's placement as the Transform fields show it.
    pub fn active_placement(&self) -> Option<Placement> {
        let id = self.active?;
        let t = self
            .shown_transform(id)
            .or_else(|| Some(self.session.document().object(id)?.transform))?;
        Some(transform_tool::placement(&t, self.object_centre(id)?))
    }

    /// The gizmo drawn over the active object while Transform is open.
    pub fn transform_gizmo(&self) -> Option<Gizmo> {
        let tool = self.transform_tool()?;
        let p = self.active_placement()?;
        let mode = if p.rotation_deg.is_none() {
            GizmoMode::Move
        } else {
            tool.mode
        };
        Some(transform_tool::gizmo(p.position, mode))
    }

    /// The handle to draw highlighted: the one dragged, else hovered.
    fn gizmo_highlight(&self) -> Option<Handle> {
        let t = self.transform_tool()?;
        t.dragging.map(|d| d.drag.handle()).or(t.hover)
    }

    /// What a press in the view hits: the gizmo, the camera it's seen
    /// with and the view's size (points). `None` while a job runs (no
    /// command could commit) or nothing is shown.
    pub(crate) fn transform_grab(&self) -> Option<(Gizmo, OrbitCamera, (f64, f64))> {
        if self.busy.is_some() {
            return None;
        }
        let rect = self.viewport.last_rect?;
        Some((
            self.transform_gizmo()?,
            self.camera?,
            (rect.width() as f64, rect.height() as f64),
        ))
    }

    /// The line at the view's top-left while Transform is open.
    fn transform_hint(&self) -> Option<&'static str> {
        self.transform_tool()?;
        Some("Transform · W move · E rotate · R scale · Cmd/Ctrl: snap off · Esc: done")
    }

    /// The build plate, sized in the document's units.
    fn plate(&self) -> mf_render::Plate {
        let mm = transform_tool::mm_per(self.session.document().units());
        let d = mf_render::Plate::default();
        mf_render::Plate {
            size_mm: d.size_mm.map(|v| v / mm),
            grid_mm: d.grid_mm / mm,
            major_mm: d.major_mm / mm,
        }
    }

    /// The snapping a drag uses now: the tool's switch, inverted while
    /// Cmd/Ctrl is held. The grid is 1 mm in document units; the plate
    /// snap needs the object's lowest point, found on a worker.
    fn transform_snap(&self, invert: bool, object: ObjectId, start: &[f64; 12]) -> Snap {
        let on = self.transform_tool().is_some_and(|t| t.snap) != invert;
        if !on {
            return Snap {
                grid: None,
                angle_deg: None,
                scale_step: None,
                plate: None,
            };
        }
        let mm = transform_tool::mm_per(self.session.document().units());
        let mesh = self
            .session
            .document()
            .object(object)
            .and_then(|o| o.mesh());
        let lowest = self
            .lowest
            .as_ref()
            .filter(|(o, m, t, _)| {
                *o == object && mesh.is_some_and(|c| Mesh::ptr_eq(c, m)) && t == start
            })
            .and_then(|(.., z)| *z);
        Snap {
            grid: Some(1.0 / mm),
            plate: lowest.map(|world_min_z| PlateSnap {
                world_min_z,
                within_pt: 8.0,
            }),
            ..Snap::default()
        }
    }

    /// Ask for the active object's lowest point at its placement (for
    /// the plate snap) while Transform is open. O(1) here; the search
    /// runs on the background pool.
    fn refresh_lowest(&mut self) {
        if self.transform_tool().is_none() {
            return;
        }
        let Some(o) = self
            .active
            .and_then(|id| self.session.document().object(id))
        else {
            return;
        };
        let Some(mesh) = o.mesh().cloned() else {
            return;
        };
        let want = (o.id, mesh, o.transform);
        let same = |a: &(ObjectId, Mesh, [f64; 12])| {
            a.0 == want.0 && Mesh::ptr_eq(&a.1, &want.1) && a.2 == want.2
        };
        if self.lowest_asked.as_ref().is_some_and(same) {
            return;
        }
        if let Some(old) = self.lowest_asked.replace(want.clone()) {
            self.workers.discard(old);
        }
        self.lowest_worker.submit(want);
    }

    /// The view's pointer for the Transform tool: a drag on a handle
    /// moves the object's drawn placement only (its model matrix); the
    /// release commits one `object.set_transform`.
    pub(crate) fn transform_pointer(
        &mut self,
        pointer: Option<ToolPointer>,
        hover: Option<(f64, f64)>,
        modifiers: egui::Modifiers,
    ) {
        let Some(grab) = self.transform_grab() else {
            if let Some(t) = self.transform_tool_mut() {
                t.hover = None;
            }
            return;
        };
        let (gizmo, cam, view) = grab;
        let invert = modifiers.command;
        match pointer {
            Some(ToolPointer::Start { from, at }) => {
                let (Some(object), Some(handle)) = (self.active, gizmo.hit(&cam, view, from))
                else {
                    return;
                };
                let Some(start) = self.session.document().object(object).map(|o| o.transform)
                else {
                    return;
                };
                let drag = gizmo.begin(handle, from, &cam, view, start);
                let snap = self.transform_snap(invert, object, &start);
                let current = drag.update(at, &snap);
                if let Some(t) = self.transform_tool_mut() {
                    t.dragging = Some(Dragging {
                        object,
                        drag,
                        start,
                        current,
                    });
                }
            }
            Some(ToolPointer::Move(px)) => self.drag_to(px, invert),
            // A click on a handle is the tool's, not a pick; Transform has
            // nothing to do with it.
            Some(ToolPointer::Click(_)) => {}
            Some(ToolPointer::End(px)) => {
                self.drag_to(px, invert);
                let Some(d) = self.transform_tool_mut().and_then(|t| t.dragging.take()) else {
                    return;
                };
                // The gizmo was drawn, so the centre is known.
                if d.current != d.start
                    && let Some(centre) = self.object_centre(d.object)
                {
                    let status = self.placement_words(d.object, centre, &d.start, &d.current);
                    self.commit_transform(d.object, d.current, status);
                }
            }
            None => {}
        }
        let hovered = if self.transform_tool().is_some_and(|t| t.dragging.is_some()) {
            None
        } else {
            hover.and_then(|px| gizmo.hit(&cam, view, px))
        };
        if let Some(t) = self.transform_tool_mut() {
            t.hover = hovered;
        }
    }

    fn drag_to(&mut self, px: (f64, f64), invert: bool) {
        let Some(d) = self.transform_tool().and_then(|t| t.dragging) else {
            return;
        };
        let snap = self.transform_snap(invert, d.object, &d.start);
        let current = d.drag.update(px, &snap);
        if let Some(Dragging { current: c, .. }) =
            self.transform_tool_mut().and_then(|t| t.dragging.as_mut())
        {
            *c = current;
        }
    }

    /// The status words for a placement change of `object`, whose
    /// bounding-box centre is `centre` (object space).
    fn placement_words(
        &self,
        object: ObjectId,
        centre: glam::DVec3,
        from: &[f64; 12],
        to: &[f64; 12],
    ) -> String {
        let doc = self.session.document();
        let name = doc.object(object).map_or("the object", |o| o.name.as_str());
        transform_tool::describe(name, doc.units(), from, to, centre)
    }

    /// Commit `transform` as object `object`'s placement: one
    /// `object.set_transform` (one undo step), run like any command. The
    /// object is drawn there meanwhile; `status` says what changed.
    pub fn commit_transform(&mut self, object: ObjectId, transform: [f64; 12], status: String) {
        if self.busy.is_some() || self.active != Some(object) {
            return;
        }
        self.preview = Some((object, transform));
        self.pending_status = Some(status);
        self.run_tool(&Tool {
            op_id: "object.set_transform".into(),
            label: "Transform".into(),
            selection: None,
            fixed: BTreeMap::from([("transform".into(), ParamValue::Affine(transform))]),
            kind: ToolKind::OneShot,
            group: None,
        });
        if self.busy.is_none() {
            // It didn't start (the error banner says why).
            self.preview = None;
            self.pending_status = None;
        }
    }

    /// Drop to plate: the active object straight down (or up) until its
    /// lowest point rests on the plate, on the worker, with progress.
    pub fn drop_to_plate(&mut self) {
        if self.busy.is_some() || self.active.is_none() {
            return;
        }
        self.run_tool(&Tool {
            op_id: "object.drop_to_plate".into(),
            label: "Drop to plate".into(),
            selection: None,
            fixed: BTreeMap::new(),
            kind: ToolKind::OneShot,
            group: None,
        });
    }

    /// Reset: the active object back to no rotation and 100% scale (a
    /// mirror kept), its bounding-box centre where it is.
    pub fn reset_transform(&mut self) {
        let Some(id) = self.active else { return };
        let (Some(o), Some(centre)) = (self.session.document().object(id), self.object_centre(id))
        else {
            return;
        };
        let from = o.transform;
        let Some(to) =
            transform_tool::with_rotation_scale(&from, centre, glam::DVec3::ZERO, glam::DVec3::ONE)
        else {
            return;
        };
        if to != from {
            let name = o.name.clone();
            self.commit_transform(id, to, format!("Reset {name}: no rotation, 100% scale"));
        }
    }

    /// A numeric field's new value, typed and committed with Enter.
    pub fn set_transform_field(&mut self, field: Field, value: f64) {
        let Some(id) = self.active else { return };
        let (Some(o), Some(centre), Some(p)) = (
            self.session.document().object(id),
            self.object_centre(id),
            self.active_placement(),
        ) else {
            return;
        };
        let from = o.transform;
        let uniform = self.transform_tool().is_some_and(|t| t.uniform);
        let to = match field {
            Field::Position(a) => {
                let mut pos = p.position;
                pos[a] = value;
                Some(transform_tool::with_position(&from, centre, pos))
            }
            Field::Rotation(a) => p.rotation_deg.zip(p.scale).and_then(|(mut r, s)| {
                r[a] = value;
                transform_tool::with_rotation_scale(&from, centre, r, s)
            }),
            Field::Scale(a) => p.rotation_deg.zip(p.scale).and_then(|(r, mut s)| {
                let f = value / 100.0;
                if !(f.is_finite() && f > 0.0) {
                    return None;
                }
                if uniform {
                    s *= f / s[a];
                } else {
                    s[a] = f;
                }
                transform_tool::with_rotation_scale(&from, centre, r, s)
            }),
        };
        match to {
            Some(to) if to != from => {
                let status = self.placement_words(id, centre, &from, &to);
                self.commit_transform(id, to, status);
            }
            Some(_) => {}
            None => {
                self.status = Some(format!(
                    "{} can't be {value}: scale is a percentage above 0",
                    field.label()
                ))
            }
        }
    }

    /// An object command's result in words, with the object's name.
    fn object_report(&self, op: &str, r: &serde_json::Value) -> Option<String> {
        let rec = self.session.node(self.session.current())?.record.as_ref()?;
        let doc = self.session.document();
        let name = doc
            .object(rec.target)
            .map_or("the object", |o| o.name.as_str());
        match op {
            "object.drop_to_plate" => {
                let dz = r["moved_z"].as_f64()?;
                Some(format!(
                    "Dropped {name} onto the plate: moved {} {} {}",
                    crate::defects::measure(dz.abs()),
                    crate::defects::unit_symbol(doc.units()),
                    if dz < 0.0 { "down" } else { "up" }
                ))
            }
            _ => None,
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
        self.refresh_lowest();
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
        // H: the selected objects' role, Solid or Hole.
        let role = !typing
            && !ctx.egui_wants_keyboard_input()
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::H));
        if role {
            let ids = self.selected_objects();
            self.toggle_role(&ids);
        }
        if !typing && !ctx.egui_wants_keyboard_input() {
            self.tool_keys(&ctx);
        }
        self.keep_transform_target();
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
        crate::panels::status_bar(self, ui);
        crate::tool_panels::rail(self, ui);
        crate::tool_panels::tool_dock(self, ui);
        crate::panels::right_dock(self, ui);
        crate::panels::replay_window(self, &ctx);
        crate::panels::settings_window(self, &ctx);
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
        // Only while it draws an earlier revision's geometry: after a move
        // the geometry shown is current while the new placement's
        // measurements are worked out.
        let updating = self.is_analyzing() && !self.shows_current_revision();
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
        // The open tool claims the presses it wants (Transform: its
        // handles) ahead of the camera and picking.
        let tool = self.active_tool_def();
        let claims = (tool.claims)(self);
        let out = egui::CentralPanel::no_frame()
            .show(ui, |ui| {
                let drawable = !objects.is_empty();
                let input = self
                    .viewport
                    .input(ui, &mut self.camera, drawable, claims.as_deref());
                let modifiers = ui.input(|i| i.modifiers);
                (tool.viewport_input)(
                    self,
                    &crate::shell::ToolInput {
                        pointer: input.out.tool,
                        hover: input.out.tool_hover,
                        modifiers,
                    },
                );
                let mut objects = objects;
                for o in &mut objects {
                    if let Some(t) = self.shown_transform(o.id) {
                        o.transform = t;
                    }
                }
                let gizmo = self.tool_gizmo();
                let plate = self.tool_hides_defects().then(|| self.plate());
                let hint = self.transform_hint();
                let split_preview = crate::shape_tools::split_preview(self);
                let shading = crate::shape_tools::shading(self);
                self.viewport.paint(
                    ui,
                    input,
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
                        gizmo,
                        plate,
                        tool_hint: hint,
                        shading,
                        split_preview,
                    },
                )
            })
            .inner;
        match out.action {
            Some(NavAction::FrameSelection) => self.frame_target(),
            Some(NavAction::ResetView) => self.fit_camera(),
            Some(NavAction::Overlays) => self.show_overlays = !self.show_overlays,
            Some(NavAction::Wireframe) => self.shading.wireframe = !self.shading.wireframe,
            None => {}
        }
        crate::tool_panels::inspector_card(self, &ctx);
        crate::tool_panels::first_use_hint(self, &ctx);
        crate::panels::overlays_popover(self, &ctx);
        crate::shape_tools::view_overlay(self, &ctx);
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
        if self.hover.is_none() && !self.tool_hides_defects() {
            crate::panels::view_hover_tooltip(self, &ctx);
        }

        let overlays_pending = self.overlays.values().any(|s| s.ready.is_none());
        if self.busy.is_some()
            || self.is_analyzing()
            || self.is_remeasuring()
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
    if id == transform_tool::ID {
        return "Transform".into();
    }
    operation_label(id, None).expect("every listed tool has plain words")
}

/// History words for the object operations (they have no Properties
/// button, so no `operation_label`).
fn object_step_label(op_id: &str) -> Option<String> {
    Some(
        match op_id {
            "object.delete" => "Delete object",
            "object.set_transform" => "Transform",
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
        "object.drop_to_plate" if report["moved_z"].as_f64() == Some(0.0) => {
            "it already rests on the plate".into()
        }
        "object.set_transform" => "the object is already placed there".into(),
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
