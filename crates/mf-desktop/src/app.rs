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
use crate::defects::{
    Defect, HoleNumbers, HoleView, count, measure, operation_label, param_label, thousands,
};
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
    /// Where Parts/Connectors read and write the user's library and
    /// Fit class/Machine profile settings (`None`: the platform
    /// directory, `mf_parts::dirs::meshfixer_dir`). Tests that save a
    /// part or a profile set this to a temp directory, so they never
    /// touch the real user's settings: `std::env::set_var` is `unsafe`
    /// in this edition and the workspace forbids unsafe code, so the
    /// usual env-var override isn't available to them.
    pub config_dir: Option<PathBuf>,
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
    /// A Browser group (folder): selecting it selects every member
    /// (project organisation brief, step 2), so Move, Hide, Export and
    /// Delete act on the whole group.
    Group(mf_command::GroupId),
}

/// One row of the History panel's grouped view
/// ([`MeshFixerApp::grouped_breadcrumb`]): a plain step, or a run of
/// consecutive small steps on the same object collapsed into one
/// expandable row ("Sculpted head (24 strokes)", Task "Project
/// organisation" step 3).
#[derive(Debug, Clone)]
pub enum BreadcrumbRow {
    Single {
        id: RevisionId,
        label: String,
        details: String,
        reached: bool,
    },
    Run {
        /// Every revision the run collapses, in order; clicking the row
        /// checks out the last one (same as the un-collapsed rows would).
        ids: Vec<RevisionId>,
        label: String,
        reached: bool,
    },
    /// The first row of a run the caller expanded ([`MeshFixerApp::expand_run`]):
    /// a "Collapse" control, shown just before that run's individual
    /// [`Self::Single`] rows.
    CollapseControl { run_id: RevisionId },
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

/// What Parts/Connectors asked to do once a placement they started
/// (`object.add_part`) actually commits (see
/// [`MeshFixerApp::pending_open_transform`]).
#[derive(Debug, Clone)]
pub enum PendingPlacement {
    /// A library part was placed on `scan`.
    Part { scan: ObjectId },
    /// A connector was placed with these recorded parameters (so Merge
    /// can run `connector.join` without reading the object's live
    /// state).
    Connector {
        connector_id: String,
        connector_params_json: String,
        gap: f64,
        /// The linked group its members were placed in.
        group: String,
    },
}

/// A command the app sent to a worker (or queued), with the tool that
/// issued it (C16: its refusal is routed to that tool's panel).
struct Issued {
    inv: Invocation,
    label: String,
    by: &'static str,
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
    /// View ▸ Colour objects / Clay (UI plan §11): each object drawn in
    /// its own palette shade, so a multi-object model stands apart from
    /// the UI chrome and from its siblings. On by default; off (Clay)
    /// keeps every object the one neutral surface colour. A per-face 3MF
    /// material or colour always overrides this (`mf_render::DrawItem`'s
    /// `object_color` is only used where the mesh has none of its own).
    pub object_colors: bool,
    /// Every gizmo tool's snap switch (owner, 2026-11-14: "can't we
    /// disable snap?") — one shared preference for the Transform gizmo
    /// and Split's plane and print-volume box, each showing it as its
    /// own "Snap: 1 mm"-style checkbox (`transform_tool::snap_step` for
    /// the live step shown). On by default; Cmd/Ctrl while dragging
    /// inverts it for that drag, in every tool
    /// ([`MeshFixerApp::gizmo_snap_on`]).
    pub gizmo_snap: bool,
    /// The Shape tools' state (UI plan step 5): Split's plane, Combine's
    /// tool and operation, Make solid's settings.
    pub split: crate::shape_tools::SplitState,
    /// Split's three-step workflow: the step, the print-volume box, the
    /// joints a cut recorded and the connectors placed on them.
    pub split_flow: crate::split_flow::Flow,
    /// Split's Alignment section (Task 13): pegs mode, shape, sizes.
    pub alignment: crate::shape_tools::AlignmentState,
    /// The Alignment panel's hole preview for the plane and settings last
    /// asked for ([`worker::peg_preview`]); `None` before the first answer,
    /// or once pegs are off.
    peg_preview: Option<Arc<worker::PegPreview>>,
    peg_preview_worker: LatestWorker<worker::PegPreviewRequest>,
    /// What was last asked for (everything a `PegPreviewRequest` carries
    /// but the `Mesh`, which has no `PartialEq`: MF-DOC-001).
    peg_preview_asked: Option<crate::shape_tools::PegPreviewKey>,
    pub combine: crate::shape_tools::CombineState,
    /// Objects selected together (Shift-click in the view or the
    /// Browser), in the order they were picked; the active object is one
    /// of them. Empty (or one) means just the active object. Combine and
    /// Split act on all of them; H toggles their roles.
    pub picked: Vec<ObjectId>,
    /// The Sculpt tool's brush panel settings (Phase 1 Task 19).
    pub sculpt: crate::sculpt_tool::SculptSettings,
    /// A stroke in progress (press to release): `None` outside Sculpt, or
    /// between strokes.
    pub sculpt_stroke: Option<crate::sculpt_tool::SculptStroke>,
    /// The just-released stroke's last live render, kept on screen after
    /// commit until the committed revision's own GPU mesh and analysis are
    /// ready (owner: no flicker back to the pre-stroke mesh, and no
    /// "Updating view…" over it meanwhile). Cleared the frame
    /// `shows_current_revision()` turns true for this object, which swaps
    /// in the real thing in the same frame.
    pub sculpt_pending: Option<(ObjectId, std::sync::Arc<crate::worker::RenderMesh>)>,
    /// Where the pointer is over the view while Sculpt is open (points
    /// from the view's top-left): the brush cursor, and where the next
    /// dab samples.
    pub sculpt_hover: Option<(f64, f64)>,
    /// The Sculpt panel's "Show sculpt changes" overlay (off by default):
    /// tints vertices by how far they've moved since the tool was opened.
    pub sculpt_show_changes: bool,
    /// The radius panel's typed-unit field while it has focus (ADR-0006
    /// rule 1b: "3 mm" and the like): `None` shows the live "≈ X at this
    /// zoom" readout instead. Mirrors [`crate::transform_tool::TransformTool::editing`]'s
    /// pattern — the text a person is typing, not yet committed.
    pub sculpt_radius_text: Option<String>,
    /// The active object's mesh as it was when Sculpt was opened (the
    /// "Show sculpt changes" overlay's reference point): `None` outside
    /// Sculpt. Reset whenever the active object changes while Sculpt
    /// stays open, so switching objects starts a fresh baseline.
    pub sculpt_baseline: Option<(ObjectId, Mesh)>,
    pub solid: crate::shape_tools::SolidState,
    /// The Hollow tool's settings and preview (MF-ISSUE-112).
    pub hollow: crate::hollow_tool::HollowState,
    /// Works out Hollow's preview on a worker, the latest settings only.
    pub(crate) hollow_worker: LatestWorker<crate::hollow_tool::PreviewRequest>,
    /// The Parts tool's state (Task 23): search, the part last placed
    /// and its Join-to-scan settings.
    pub parts: crate::parts_tool::PartsState,
    /// The Connectors tool's state (Task 29): search, the fit class and
    /// machine profile pickers, and the connector last placed.
    pub connectors: crate::connector_tool::ConnectorsState,
    /// The editable shape's params being edited in the selection panel.
    pub shape_edit: Option<crate::shape_panel::ShapeDraft>,
    /// Hinge placement choices and the swing preview.
    pub hinge: crate::hinge_tool::HingeState,
    /// Editable holes whose profile a move left stale, to re-resolve once
    /// idle (`shape_panel::after_commit`).
    pub pending_reorient: Option<Vec<(ObjectId, mf_parts::GeneratorRecord)>>,
    /// Set right after importing at "Scale to fit plate" (MF-ISSUE-045):
    /// the newly imported object and the uniform factor to scale it by,
    /// applied on the next idle frame as its own undoable
    /// `object.set_transform` step (import itself is a separate step,
    /// so Undo first restores the file's own size, then removes the
    /// import, matching the brief's "Record the scaling as its own
    /// undoable step").
    pub pending_scale_to_fit: Option<(ObjectId, f64)>,
    /// Set by Parts/Connectors right before placing (`object.add_part`):
    /// once that command's `Message::Completed` has run (so `active` is
    /// already the newly created object, D6's own `after_object_changes`
    /// rule), the next frame opens Transform on it, records the
    /// placement in the tool's own state, and clears this. Placing is
    /// asynchronous (the job pool), so none of that can happen inside
    /// the call that starts it.
    pub pending_open_transform: Option<PendingPlacement>,
    /// The command [`Self::run_invocation`] actually sent to a worker
    /// (its label, for the status/error text), kept so that if it comes
    /// back `StaleInput` only because this app's own next queued command
    /// already committed in between, it can be re-prepared on the new
    /// revision and re-run (I5 single-queue fix): operations are
    /// deterministic, so replaying ours against our own later revision
    /// is always safe, and the error is shown only when the document's
    /// own content makes the command invalid. It also records the tool
    /// that issued it (the active tool when it was asked for, C16): its
    /// refusal goes to that tool's panel, never to whichever tool
    /// happens to be open when the result lands.
    in_flight_invocation: Option<Issued>,
    /// Later commands asked for while one is already running, in order,
    /// run one at a time once the running one finishes. A command for
    /// the same op and target as the *last* queued one replaces it
    /// (coalescing: a slider drag's last value wins over its earlier
    /// ones), so rapid edits leave one pending step, not a backlog; any
    /// other command queues behind it and is never dropped (final
    /// review I2: a single overwritten slot lost queued edits silently).
    pending_invocation: std::collections::VecDeque<Issued>,
    /// The last operation refusal, shown in its tool's panel in plain
    /// words with the next action.
    pub refusal: Option<crate::shape_tools::Refusal>,
    /// The tool that issued the command `refusal` came back from (C16),
    /// kept with the refusal it belongs to: a refusal a panel built
    /// itself has no issuer and isn't matched by a stale one.
    refusal_issuer: Option<(crate::shape_tools::Refusal, &'static str)>,
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
    /// Inspect's Units control (import-units-brief.md, "Fix it
    /// afterwards"): the unit picked for "Reinterpret" or "Scale",
    /// `None` until the person opens the picker (the document's own
    /// unit is shown as the field's starting value, not stored here).
    pub inspect_units: Option<mf_units::Unit>,
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
    /// Where Parts/Connectors read and write settings
    /// ([`Settings::config_dir`]).
    pub(crate) config_dir: Option<PathBuf>,
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
    /// View ▸ Overlays ▸ "Show holes through solids" (on by default,
    /// Tinkercad-style): a Hole's hidden part — buried inside or behind a
    /// solid — draws as a fainter ghost of its hatch.
    pub show_holes_through_solids: bool,
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
    /// Finds every Solid/Hole pair's exact crossing curve on the
    /// background pool (`mesh.intersections`): a newer request (roles,
    /// transforms, or a Transform drag) cancels the one running, and the
    /// previous curves stay shown until the new ones arrive.
    intersection_worker: LatestWorker<worker::IntersectionRequest>,
    /// What the curves were last asked for, and its request number.
    intersection_query: Option<(IntersectionQuery, u64)>,
    /// The latest curves found (request number, one entry per overlapping
    /// pair: a pair that doesn't cross has none).
    intersections: Option<(u64, Vec<worker::IntersectionCurve>)>,
    /// A name edit in progress (the selection panel's name field, F2 on
    /// the active object, or the Browser row's "Rename…" context menu
    /// item): which object, and the text so far. Committed on Enter or
    /// losing focus; Esc cancels it.
    pub renaming: Option<(ObjectId, String)>,
    /// The Browser's search field (project organisation brief, step 2):
    /// filters rows by name; empty shows everything.
    pub browser_search: String,
    /// A checkpoint name edit in progress (History row's right-click
    /// "Name this point…"): which revision, and the text so far.
    /// Committed on Enter or losing focus; Esc cancels it (Task
    /// "Project organisation" step 3).
    pub checkpoint_naming: Option<(RevisionId, String)>,
    /// The collapsed run whose individual steps are shown expanded
    /// (`grouped_breadcrumb`'s row, keyed on its first revision): `None`
    /// keeps every eligible run collapsed.
    pub expanded_run: Option<RevisionId>,
    /// The Align tool's state (Phase 1 modelling tools, Tinkercad-style
    /// Align): the anchor, the hovered dot, and the Distribute choices
    /// per axis.
    pub align: crate::align_tool::AlignState,
}

/// What the hole overview's markers depend on: a change asks again.
#[derive(Debug, Clone, Copy, PartialEq)]
struct MarkerQuery {
    camera: OrbitCamera,
    rect: egui::Rect,
    defects: usize,
    view: usize,
}

/// What the Solid/Hole intersection curves depend on: each object's
/// revision, transform (as shown — including a live Transform drag) and
/// role. A change asks again; the previous curves stay shown meanwhile
/// (MF-RENDER's "keep the previous curve shown until the new one
/// arrives").
#[derive(Debug, Clone)]
struct IntersectionQuery {
    /// (object, mesh revision, transform bits, is_hole), sorted by object.
    objects: Vec<(ObjectId, Mesh, [u64; 12], bool)>,
}

impl IntersectionQuery {
    /// True if both ask for the same thing: same objects, in the same
    /// order, each at the same revision (by identity, not content),
    /// transform and role.
    fn same_as(&self, other: &IntersectionQuery) -> bool {
        self.objects.len() == other.objects.len()
            && self.objects.iter().zip(&other.objects).all(
                |((id, mesh, t, hole), (id2, mesh2, t2, hole2))| {
                    id == id2 && Mesh::ptr_eq(mesh, mesh2) && t == t2 && hole == hole2
                },
            )
    }
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
        let intersection_worker = {
            let (tx, ctx, gpu) = (tx.clone(), cc.egui_ctx.clone(), gpu.clone());
            LatestWorker::new(
                workers.background(),
                move |req: worker::IntersectionRequest, run: &RunContext| {
                    worker::intersections(req, run, &gpu)
                },
                move |curves| {
                    // allow-swallow: the receiver only disappears while the app is closing
                    let _ = tx.send(Message::IntersectionsFound(curves));
                    ctx.request_repaint();
                },
            )
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
        let hollow_worker = {
            let (tx, ctx, gpu) = (tx.clone(), cc.egui_ctx.clone(), gpu.clone());
            LatestWorker::new(
                workers.background(),
                move |req: crate::hollow_tool::PreviewRequest, run: &RunContext| {
                    crate::hollow_tool::preview_work(req, run, &gpu)
                },
                move |msg| {
                    // allow-swallow: the receiver only disappears while the app is closing
                    let _ = tx.send(msg);
                    ctx.request_repaint();
                },
            )
        };
        let peg_preview_worker = {
            let (tx, ctx) = (tx.clone(), cc.egui_ctx.clone());
            LatestWorker::new(workers.background(), worker::peg_preview, move |msg| {
                // allow-swallow: the receiver only disappears while the app is closing
                let _ = tx.send(msg);
                ctx.request_repaint();
            })
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
            object_colors: true,
            gizmo_snap: true,
            split: Default::default(),
            split_flow: Default::default(),
            alignment: Default::default(),
            peg_preview: None,
            peg_preview_worker,
            hollow: Default::default(),
            hollow_worker,
            peg_preview_asked: None,
            combine: Default::default(),
            picked: Vec::new(),
            sculpt: Default::default(),
            sculpt_stroke: None,
            sculpt_pending: None,
            sculpt_hover: None,
            sculpt_show_changes: false,
            sculpt_radius_text: None,
            sculpt_baseline: None,
            solid: Default::default(),
            parts: Default::default(),
            connectors: Default::default(),
            shape_edit: None,
            hinge: Default::default(),
            pending_reorient: None,
            pending_scale_to_fit: None,
            pending_open_transform: None,
            in_flight_invocation: None,
            pending_invocation: std::collections::VecDeque::new(),
            refusal: None,
            refusal_issuer: None,
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
            inspect_units: None,
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
            config_dir: settings.config_dir,
            unit_prompt: None,
            confirm: None,
            hole_numbers: BTreeMap::new(),
            hole_list: None,
            hole_list_requested: None,
            show_hole_overview: false,
            show_hole_markers: true,
            show_holes_through_solids: true,
            side_panels_hidden: false,
            marker_worker,
            marker_query: None,
            markers: None,
            reveal: None,
            intersection_worker,
            intersection_query: None,
            intersections: None,
            renaming: None,
            browser_search: String::new(),
            checkpoint_naming: None,
            expanded_run: None,
            align: Default::default(),
        };
        if let Some(path) = initial {
            app.open(path);
        }
        Ok(app)
    }

    /// Where Parts/Connectors read and write settings
    /// ([`Settings::config_dir`], or the platform directory).
    pub fn settings_dir(&self) -> Option<PathBuf> {
        match &self.config_dir {
            Some(dir) => Some(dir.clone()),
            // allow-swallow: no settings folder (an unset HOME, a sandboxed test environment) means every picker falls back to its own built-in defaults, the documented behaviour, not a hidden error.
            None => mf_parts::dirs::meshfixer_dir().ok(),
        }
    }

    pub fn active_mesh(&self) -> Option<&Mesh> {
        self.active
            .and_then(|id| self.session.document().object(id))
            .and_then(|o| o.mesh())
    }

    /// The [`mf_units::ScaleCtx`] a panel's `length_field`/`length_slider`
    /// uses (consolidation plan C15, units-audit.md §6.5): the document's
    /// unit as both doc and display (the app has no separate
    /// display-unit preference yet), and `bbox_diag` from the active
    /// object so `length_slider`'s range is bbox-relative. No live
    /// camera is threaded through here (panel fields aren't a viewport
    /// drag), so `world_per_pt` is `None` and the scrub step falls back
    /// to the document scale.
    pub fn scale_ctx(&self) -> mf_units::ScaleCtx {
        let doc = self.session.document().units();
        let bbox_diag = self
            .active_mesh()
            .and_then(|m| m.bounding_box())
            .map(|b| b.diagonal())
            .unwrap_or(0.0);
        mf_units::ScaleCtx {
            doc,
            display: doc,
            bbox_diag,
            world_per_pt: None,
        }
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
                // "Active Machine profile" means one the user picked: the
                // built-in Uncalibrated profile (always active until they
                // choose otherwise) counts as "no profile", so an
                // unconfigured install checks against the plain 250 mm
                // default plate rather than Uncalibrated's own volume.
                let (profiles, profile_err) = crate::connector_tool::machine_profiles(self);
                let active = (profile_err.is_none()
                    && profiles.active != mf_parts::UNCALIBRATED_NAME)
                    .then(|| profiles.active_profile());
                let plate = crate::import_dialog::Plate::from_profile(active);
                let hint = model.source.exporter_hint.clone();
                let (memory, memory_err) = crate::import_dialog::import_memory(self);
                if let Some(e) = memory_err {
                    self.warnings.push(format!(
                        "Your remembered import units couldn't be read ({e}); asking fresh."
                    ));
                }
                self.unit_prompt = Some(UnitPrompt::new(name, extent, model, plate, hint, &memory));
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
            Some((true, scale_to_fit)) => {
                if let Some(prompt) = self.unit_prompt.take() {
                    self.remember_import_unit(&prompt);
                    let factor = scale_to_fit.then(|| prompt.scale_to_fit_factor());
                    self.import_model(prompt.into_model());
                    if let (Some(factor), Some(id)) = (factor, self.active) {
                        self.pending_scale_to_fit = Some((id, factor));
                    }
                }
            }
            Some((false, _)) => {
                if let Some(prompt) = self.unit_prompt.take() {
                    self.workers.discard(prompt);
                }
                self.status = Some("Import cancelled: nothing opened".into());
            }
        }
    }

    /// Records the unit `prompt` was confirmed with (its final
    /// [`UnitPrompt::choice`], after any dialog edits) against its
    /// folder and exporter signature, so the next import from either
    /// preselects it (import-units-brief.md, "Remember choices"). Never
    /// an error the user sees: a settings directory that can't be
    /// written just means the next import asks fresh, same as today.
    fn remember_import_unit(&self, prompt: &UnitPrompt) {
        let Some(dir) = self.settings_dir() else {
            return;
        };
        let (mut memory, _) = crate::import_dialog::import_memory(self);
        memory.remember(
            prompt.folder.as_deref(),
            prompt.exporter_signature,
            prompt.choice,
        );
        let _ = memory.save(&dir); // allow-swallow: a failed save leaves this import's memory for this session only; the next import retries the write
    }

    /// Applies a "Scale to fit plate" import's scaling, once the import
    /// itself has committed and nothing else is busy: a uniform scale
    /// of the newly imported object's transform, as its own undoable
    /// step (`commit_transform`), so Undo first restores the file's own
    /// size, then removes the import.
    fn run_pending_scale_to_fit(&mut self) {
        if self.busy.is_some() {
            return;
        }
        let Some((id, factor)) = self.pending_scale_to_fit.take() else {
            return;
        };
        let Some(current) = self.session.document().object(id).map(|o| o.transform) else {
            return; // the object was removed (e.g. the import was undone)
        };
        let scaled = transform_tool::scale_linear(&current, factor);
        self.active = Some(id);
        self.commit_transform(id, scaled, "Scale to fit plate".into());
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
        // The Browser's organisation and the saved object ids, only when
        // there's something to say: a plain document with everything at
        // the root exports with no MeshFixer model metadata at all.
        Ok(mf_io::threemf::Model {
            metadata: doc.model_metadata(),
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
        let intent = crate::intent::Intent::op(&tool.op_id)
            .on(target)
            .selection(tool.selection.clone())
            .values(&params);
        self.run_intent(intent, tool.label.clone());
    }

    /// The history breadcrumb: the current path from the first import, then
    /// the steps redo would take (greyed). Root ("New document") is left
    /// out. Each row's short label is a plain sentence; its details are
    /// every parameter the command ran with that isn't hidden or left at
    /// its default (I5, owner 2026-11-11), for a hover tooltip.
    pub fn breadcrumb(&self) -> Vec<(RevisionId, String, String, bool)> {
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
                Some((
                    id,
                    step_label(&self.session, node),
                    history_details(node),
                    reached,
                ))
            })
            .collect()
    }

    /// Consecutive breadcrumb rows of the same small, repeatable
    /// operation on the same object (sculpt strokes, transform nudges)
    /// collapse into one: "Sculpted head (24 strokes)" (Task "Project
    /// organisation" step 3). A lone step, or a run the caller has
    /// expanded (`expanded_run`), is left as its own rows.
    pub fn grouped_breadcrumb(&self) -> Vec<BreadcrumbRow> {
        let flat = self.breadcrumb();
        let mut out = Vec::new();
        let mut i = 0;
        while i < flat.len() {
            let run_len = self.run_length(&flat, i);
            if run_len > 1 && self.expanded_run != Some(flat[i].0) {
                let ids: Vec<RevisionId> = flat[i..i + run_len].iter().map(|r| r.0).collect();
                let reached = flat[i + run_len - 1].3;
                let noun = self.run_noun(flat[i].0);
                let name = self.run_object_name(flat[i].0);
                out.push(BreadcrumbRow::Run {
                    ids,
                    label: format!(
                        "{noun} {name} ({run_len} {})",
                        self.run_unit(flat[i].0, run_len)
                    ),
                    reached,
                });
                i += run_len;
            } else if run_len > 1 {
                // Expanded: a "Collapse" control, then its own rows.
                out.push(BreadcrumbRow::CollapseControl { run_id: flat[i].0 });
                for (id, label, details, reached) in &flat[i..i + run_len] {
                    out.push(BreadcrumbRow::Single {
                        id: *id,
                        label: label.clone(),
                        details: details.clone(),
                        reached: *reached,
                    });
                }
                i += run_len;
            } else {
                let (id, label, details, reached) = flat[i].clone();
                out.push(BreadcrumbRow::Single {
                    id,
                    label,
                    details,
                    reached,
                });
                i += 1;
            }
        }
        out
    }

    /// How many consecutive rows from `start` share a collapsible
    /// operation and target (a run `grouped_breadcrumb` folds into one).
    fn run_length(&self, flat: &[(RevisionId, String, String, bool)], start: usize) -> usize {
        let Some(key) = self.run_key(flat[start].0) else {
            return 1;
        };
        let mut n = 1;
        while start + n < flat.len() && self.run_key(flat[start + n].0) == Some(key) {
            n += 1;
        }
        n
    }

    /// `(op_id, target)` if revision `id`'s command is one
    /// `grouped_breadcrumb` ever collapses with its neighbours
    /// (currently: sculpt strokes and a plain `object.set_transform`
    /// with no other document change).
    fn run_key(&self, id: RevisionId) -> Option<(&'static str, ObjectId)> {
        let rec = self.session.node(id)?.record.as_ref()?;
        match rec.op_id.as_str() {
            "mesh.sculpt_stroke" => Some(("mesh.sculpt_stroke", rec.target)),
            "object.set_transform" => Some(("object.set_transform", rec.target)),
            _ => None,
        }
    }

    fn run_noun(&self, id: RevisionId) -> &'static str {
        match self.run_key(id) {
            Some(("mesh.sculpt_stroke", _)) => "Sculpted",
            Some(("object.set_transform", _)) => "Nudged",
            _ => "Changed",
        }
    }

    fn run_unit(&self, id: RevisionId, n: usize) -> String {
        let word = match self.run_key(id) {
            Some(("mesh.sculpt_stroke", _)) => "stroke",
            _ => "move",
        };
        format!("{word}{}", plural_s(n))
    }

    fn run_object_name(&self, id: RevisionId) -> String {
        let Some((_, target)) = self.run_key(id) else {
            return "the object".into();
        };
        self.session
            .node(id)
            .and_then(|n| n.document.object(target))
            .map_or_else(|| "the object".into(), |o| o.name.clone())
    }

    /// Expands one collapsed run (clicking it in the History panel), so
    /// its individual steps show as their own rows again; `None` keeps
    /// every eligible run collapsed.
    pub fn expand_run(&mut self, id: Option<RevisionId>) {
        self.expanded_run = id;
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
        if crate::align_tool::anchor_click(self, click.px) {
            return;
        }
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
        let org = self.session.document().organisation();
        self.session
            .document()
            .objects()
            .iter()
            // Hidden objects don't render or pick; locked ones can't be
            // selected in the view (Task "Project organisation" step 2).
            .filter(|o| !org.is_hidden(o.id) && !org.is_locked(o.id))
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
    /// The picking BVH of object `id`, if it's built for `mesh` (that
    /// revision of it).
    pub fn object_bvh(&self, id: ObjectId, mesh: &Mesh) -> Option<Arc<Bvh>> {
        self.bvhs
            .get(&id)
            .filter(|(m, _)| Mesh::ptr_eq(m, mesh))
            .map(|(_, b)| b.clone())
    }

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

    /// 1 while a command is queued behind the one running (I5's single
    /// command queue), 0 otherwise: tests use this to wait out a
    /// coalesced run of rapid edits without caring how many were asked
    /// for, since only the latest is ever kept.
    pub fn pending_invocation_depth(&self) -> usize {
        self.pending_invocation.len()
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

    /// Object `id`'s picking BVH, if it's built for its current revision
    /// (Sculpt reuses it for dab targeting and [`mf_operations::sculpt::SculptIndex`],
    /// rather than building its own).
    pub fn picking_bvh(&self, id: ObjectId) -> Option<Arc<Bvh>> {
        let mesh = self.session.document().object(id)?.mesh()?;
        self.bvhs
            .get(&id)
            .filter(|(m, _)| Mesh::ptr_eq(m, mesh))
            .map(|(_, b)| b.clone())
    }

    /// The device the view's GPU buffers are built on (Sculpt's live
    /// preview, `sculpt_tool.rs`: it syncs its own `GpuMesh` on the UI
    /// thread, same device, no worker hand-off for one dab).
    pub fn gpu_device(&self) -> wgpu::Device {
        self.viewport.device().0.clone()
    }

    pub fn gpu_queue(&self) -> wgpu::Queue {
        self.viewport.device().1.clone()
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
        // A sculpt stroke mid-drag is dropped, not committed (unlike
        // closing the tool otherwise, `open_rail_tool`'s always-Commit):
        // Task 19's Esc semantics.
        if self.sculpt_stroke.take().is_some() {
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
    /// status bar), as a whole-object tool's Apply does. The single choke
    /// point every command (a tool's Apply, a slider or drag commit, an
    /// auto re-aim, `move_group`...) goes through: with a command already
    /// running, `inv` is queued instead of raced against it (in order;
    /// coalesced with the last queued command only when that command's
    /// own spec declares `coalesce: absolute` (ADR-0009) and the key —
    /// same op, same target — matches, latest params winning), and run
    /// once the running one's result has been handled
    /// (`drain_pending_invocation`). This is what keeps two app-issued
    /// commands from ever preparing against the same base and
    /// committing out of order (owner-reported "document changed while
    /// the command ran"), without silently dropping a delta command
    /// (`mesh.sculpt_stroke`'s default `coalesce: never`): three quick
    /// strokes queued behind a running one are three undo steps, not
    /// fewer.
    pub fn run_invocation(&mut self, inv: Invocation, label: String) {
        let issued = Issued {
            inv,
            label,
            by: self.active_tool,
        };
        if self.busy.is_some() {
            let inv = &issued.inv;
            let coalesces = mf_spec::builtin::get(&inv.op_id)
                .is_some_and(|spec| spec.coalesce == mf_spec::Coalesce::Absolute);
            match self.pending_invocation.back_mut() {
                Some(queued)
                    if coalesces
                        && queued.inv.op_id == inv.op_id
                        && queued.inv.target == inv.target =>
                {
                    *queued = issued;
                }
                _ => self.pending_invocation.push_back(issued),
            }
            return;
        }
        self.start_invocation(issued);
    }

    /// Actually prepares and starts `inv`: only called with no command
    /// running, either straight from [`Self::run_invocation`] or when
    /// draining the one queued command.
    fn start_invocation(&mut self, issued: Issued) {
        match self.session.prepare(issued.inv.clone()) {
            Ok(prepared) => {
                let run = self.start_job(format!("{}…", issued.label), true);
                self.in_flight_invocation = Some(issued);
                self.workers.run(prepared, run);
            }
            Err(CommandError::Operation(e)) if self.route_refusal(&issued, &e) => {
                self.in_flight_invocation = None;
            }
            Err(e) => {
                self.in_flight_invocation = None;
                self.error = Some(format!(
                    "{} can't run: {e}. Change the settings and Apply again.",
                    issued.label
                ))
            }
        }
    }

    /// Sends `e`, the refusal of the command `issued`, to the panel of
    /// the tool that issued it (ADR-0010, C16): `app.refusal`, rendered
    /// by `RefusalCard`, never the generic error bar. False when `e`
    /// isn't a refusal (a cancellation, a spec error).
    fn route_refusal(&mut self, issued: &Issued, e: &mf_operations::OpError) -> bool {
        let Some(r) = crate::refusal_card::Refusal::from_op_error(&issued.inv.op_id, e) else {
            return false;
        };
        self.status = Some(format!("Nothing was changed: {}", r.message));
        self.refusal_issuer = Some((r.clone(), issued.by));
        self.refusal = Some(r);
        true
    }

    /// The tool that issued the command the shown refusal came back from,
    /// if it came back from one (C16): its panel is the only one that
    /// shows it.
    pub fn refusal_issuer(&self) -> Option<&'static str> {
        match (&self.refusal, &self.refusal_issuer) {
            (Some(r), Some((issued, by))) if r == issued => Some(by),
            _ => None,
        }
    }

    /// Once a command's result has been handled, start whatever was
    /// queued behind it (if anything): called from the same place every
    /// time, so a coalesced drag or a follow-up (auto re-aim) always
    /// eventually runs, never dropped by the guard that kept it from
    /// racing the command in front of it.
    fn drain_pending_invocation(&mut self) {
        if self.busy.is_some() {
            return;
        }
        if let Some(issued) = self.pending_invocation.pop_front() {
            self.start_invocation(issued);
        }
    }

    /// If `e` is only `StaleInput` because this app's own queue committed
    /// `in_flight`'s base revision out from under it (never because of an
    /// external change: this app is the only writer of its own document),
    /// re-prepare `in_flight` on the document's current revision and run
    /// it again. Operations are deterministic, so replaying our own
    /// command on our own later revision is safe; the caller sees the
    /// error only when it is the document's own content that refuses the
    /// command, not a base revision this app raced against itself.
    fn retry_self_inflicted_stale(&mut self, e: &CommandError) -> bool {
        let CommandError::StaleInput { actual, .. } = e else {
            return false;
        };
        if self.session.current().0 != *actual {
            // Not (or no longer) at the revision the failure named: don't
            // guess at a moving target.
            return false;
        }
        let Some(issued) = self.in_flight_invocation.take() else {
            return false;
        };
        self.start_invocation(issued);
        true
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
            // Sculpt's "Show sculpt changes" measures from a snapshot of
            // the active object: a fresh one for the newly active object,
            // so switching objects doesn't compare the wrong mesh (or
            // silently disable the overlay).
            if self.active_tool == crate::sculpt_tool::SCULPT {
                self.sculpt_baseline = self
                    .session
                    .document()
                    .object(id)
                    .and_then(|o| o.mesh())
                    .map(|m| (id, m.clone()));
            }
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

    /// `id`'s colour swatch for the Browser row (the same colour its
    /// viewport draw uses, [`object_color_for`]): `None` for a Hole (its
    /// row shows the neutral Hole icon instead of a colour) or if the
    /// Colour objects view is off.
    pub fn object_swatch(&self, id: ObjectId) -> Option<egui::Color32> {
        let doc = self.session.document();
        let o = doc.object(id)?;
        let mesh = o.mesh()?;
        if !self.object_colors || o.is_hole() || mesh.has_materials() {
            return None;
        }
        let group = o.properties.get(mf_command::GROUP).map(String::as_str);
        Some(theme::object_color(o.id.0, group))
    }

    /// Toggle the Solid/Hole role of `ids` (H, or the Browser's menu) as
    /// one undoable step: all become solids if every one is a hole,
    /// otherwise all become holes.
    pub fn toggle_role(&mut self, ids: &[ObjectId]) {
        if self.busy.is_some() || ids.is_empty() || ids.iter().any(|&id| self.is_locked(id)) {
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

    /// Every object a Browser row's item stands for: a group selects all
    /// its members (recursively), an object just itself.
    pub fn entry_objects(&self, entry: mf_command::Entry) -> Vec<ObjectId> {
        match entry {
            mf_command::Entry::Object(id) => vec![id],
            mf_command::Entry::Group(g) => self.session.document().organisation().members(g),
        }
    }

    /// True if `id` is hidden (doesn't render or pick): the Browser's
    /// show/hide toggle, document state.
    pub fn is_hidden(&self, id: ObjectId) -> bool {
        self.session.document().organisation().is_hidden(id)
    }

    /// True if `id` is locked (can't be selected in the view or edited):
    /// the Browser's lock toggle, document state.
    pub fn is_locked(&self, id: ObjectId) -> bool {
        self.session.document().organisation().is_locked(id)
    }

    /// Sets `ids`' hidden flag as one undo step (the Browser's eye icon,
    /// or a group's, which hides every member). All become hidden if any
    /// isn't yet, otherwise all become shown (matching `toggle_role`'s
    /// own all-or-nothing rule).
    pub fn set_hidden(&mut self, ids: &[ObjectId], hidden: bool) {
        if self.busy.is_some() || ids.is_empty() {
            return;
        }
        let mut org = self.session.document().organisation().clone();
        for &id in ids {
            org = org.with_hidden(id, hidden);
        }
        let label = if hidden { "Hide" } else { "Show" }.to_owned();
        if self.session.set_organisation(org, label.clone()) {
            self.after_history_move();
            self.status = Some(label);
        }
    }

    /// Sets `ids`' locked flag as one undo step (the Browser's padlock).
    pub fn set_locked(&mut self, ids: &[ObjectId], locked: bool) {
        if self.busy.is_some() || ids.is_empty() {
            return;
        }
        let mut org = self.session.document().organisation().clone();
        for &id in ids {
            org = org.with_locked(id, locked);
        }
        let label = if locked { "Lock" } else { "Unlock" }.to_owned();
        if self.session.set_organisation(org, label.clone()) {
            self.after_history_move();
            self.status = Some(label);
        }
    }

    /// Groups the selected objects (Cmd+G): a new folder holding them, in
    /// the selection's own order, one undo step. Does nothing with fewer
    /// than one object selected.
    pub fn group_selection(&mut self) {
        if self.busy.is_some() {
            return;
        }
        let ids = self.selected_objects();
        if ids.is_empty() {
            return;
        }
        let org = self.session.document().organisation().clone();
        let entries: Vec<mf_command::Entry> = ids
            .iter()
            .map(|&id| mf_command::Entry::Object(id))
            .collect();
        let (org, _group) = org.with_new_group("Group".to_owned(), &entries);
        if self.session.set_organisation(org, "Group".to_owned()) {
            self.after_history_move();
            self.status = Some("Group".to_owned());
        }
    }

    /// Moves `entry` to sit at `index` of `into`'s order (`None`: the
    /// root): the Browser row's Move up/down/out-of-group menu, one undo
    /// step. Does nothing if the move is refused (a cycle, or an unknown
    /// group).
    pub fn move_entry(
        &mut self,
        entry: mf_command::Entry,
        into: Option<mf_command::GroupId>,
        index: usize,
    ) {
        if self.busy.is_some() {
            return;
        }
        let org = self.session.document().organisation().clone();
        let Ok(org) = org.with_moved(entry, into, index) else {
            return;
        };
        if self.session.set_organisation(org, "Reorder".to_owned()) {
            self.after_history_move();
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

    /// Delete `id`, named explicitly (the Browser row's own trash button,
    /// owner 2026-11-12: Delete a split part either with the Backspace/
    /// Delete key, which only ever acts on the active object, or this
    /// button, which acts on the row it's on regardless of which object
    /// is active): the same `object.delete` command, undoable.
    pub fn delete_object(&mut self, id: ObjectId) {
        if self.busy.is_some() || self.is_locked(id) {
            return;
        }
        self.run_intent(
            crate::intent::Intent::op("object.delete").on(id),
            "Delete object".into(),
        );
    }

    /// Rename `id` to `name` (the Browser row's F2/double-click, or the
    /// selection panel's name field): the `object.rename` command, one
    /// undo step. Blank or unchanged names are left alone (no revision).
    pub fn rename_object(&mut self, id: ObjectId, name: String) {
        let trimmed = name.trim();
        if trimmed.is_empty() || self.busy.is_some() || self.is_locked(id) {
            return;
        }
        if self
            .session
            .document()
            .object(id)
            .is_some_and(|o| o.name == trimmed)
        {
            return;
        }
        self.run_intent(
            crate::intent::Intent::op("object.rename")
                .on(id)
                .param("name", trimmed),
            "Rename object".into(),
        );
    }

    /// Names (or renames) revision `id` as a History checkpoint ("Name
    /// this point…", Task "Project organisation" step 3): not an undo
    /// step (naming a point doesn't change the document). A blank name
    /// removes it.
    pub fn name_checkpoint(&mut self, id: RevisionId, name: String) {
        self.session.name_checkpoint(id, name);
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
                // A stale base is only ever this app's own doing (it is
                // the document's only writer): if the revision the queue
                // just committed is the one this very command went stale
                // against, re-run it on the new revision instead of
                // showing the error (I5 single-queue fix). A real retry
                // starts a new job and returns before touching anything
                // below, which belongs to the command that just finished.
                if let Err(e) = &committed
                    && self.retry_self_inflicted_stale(e)
                {
                    return;
                }
                let issued = self.in_flight_invocation.take();
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
                let is_new_revision = matches!(&committed, Ok(CommitOutcome::NewRevision { .. }));
                // A refused, cancelled or no-op placement command leaves
                // nothing to open Transform on: drop the pending
                // placement right here rather than letting the frame
                // loop's drain (`pending_open_transform.take()`) act on
                // it later with no way to tell the refusal apart from a
                // commit (the owner-reported connector bug: a refused
                // `object.add_shape` still opened Transform on the scan
                // and recorded it as the placed connector).
                if !is_new_revision {
                    self.pending_open_transform = None;
                }
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
                        // Split's workflow: a cut's joints, or a joint's
                        // connectors applied.
                        crate::split_flow::after_commit(self, op, &r);
                        crate::shape_panel::after_commit(self, op);
                        crate::hollow_tool::after_commit(self, op, &r);
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
                    // Every operation's refusal goes to the panel of the
                    // tool that issued the command (ADR-0010, C16), as a
                    // RefusalCard: never the raw "code: message" in the
                    // error bar.
                    Err(CommandError::Operation(e))
                        if issued
                            .as_ref()
                            .is_some_and(|issued| self.route_refusal(issued, &e)) => {}
                    Err(e) => {
                        self.error = Some(format!(
                            "The operation failed: {e}. Nothing was changed; try other settings \
                             or another target."
                        ))
                    }
                }
                // A committed stroke (`Ok(NewRevision)`) leaves
                // `sculpt_pending` up: `refresh_analysis` clears it once
                // the real revision has swapped in, so the view never
                // drops to the stale pre-stroke mesh in between. Anything
                // else for `mesh.sculpt_stroke` (refused, failed,
                // cancelled, or a no-op) means the document's mesh never
                // changes, so nothing will ever clear it that way: drop
                // the stand-in now and let the document's actual state
                // (the pre-stroke mesh) show again.
                if op == "mesh.sculpt_stroke" && !is_new_revision {
                    self.sculpt_pending = None;
                }
                // Whatever a drag, an auto re-aim or another tool queued
                // behind this command while it ran: now that `busy` is
                // clear and the document reflects this command's result,
                // it can run without racing anything (I5 single-queue
                // fix).
                self.drain_pending_invocation();
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
            Message::IntersectionsFound(worker::IntersectionCurves { key, curves }) => {
                // The latest request only: an older, slower one that
                // finishes after a newer one must not overwrite it (the
                // throttle keeps the previous curve shown until the new
                // one arrives, never an older one after a newer).
                if self
                    .intersection_query
                    .as_ref()
                    .is_some_and(|(_, k)| *k == key)
                {
                    self.intersections = Some((key, curves));
                }
            }
            Message::HollowPreview(preview) => {
                crate::hollow_tool::on_preview(self, *preview);
            }
            Message::PegPreview(preview) => {
                // The latest request only (a slower, older one finishing
                // after a newer must not overwrite it): compared by the
                // object and settings asked for, since `PegPreview` holds
                // a `Mesh` with no `PartialEq` (MF-DOC-001).
                if self
                    .peg_preview_asked
                    .as_ref()
                    .is_some_and(|k| k.object == preview.object)
                {
                    self.peg_preview = Some(Arc::new(preview));
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
        // A stroke's stand-in render (`sculpt_pending`) only ever belongs
        // to the active object: switching away from it (or losing it
        // entirely — deleted, or undone out from under the stroke) means
        // nothing will ever ready its revision and clear this the usual
        // way, so drop it here rather than leave it drawn forever.
        if self
            .sculpt_pending
            .as_ref()
            .is_some_and(|(id, _)| Some(*id) != self.active)
        {
            self.sculpt_pending = None;
        }
        let active = self
            .active
            .and_then(|id| self.session.document().object(id));
        let Some((mesh, transform)) = active.and_then(|o| Some((o.mesh()?.clone(), o.transform)))
        else {
            if let Some(old) = self.displayed.take() {
                self.workers.discard(old);
            }
            self.sculpt_pending = None;
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
                // The real, fully-analysed revision just swapped in for
                // this object: the stroke's stand-in render
                // (`sculpt_pending`, kept on screen since release so the
                // view never reverted to the pre-stroke mesh) has done its
                // job. Drop it in this same frame, so `object_views` picks
                // up `displayed` instead of a redundant fallback — the
                // swap is a single frame either way, never a second one
                // to let go of the stand-in.
                if self
                    .sculpt_pending
                    .as_ref()
                    .is_some_and(|(id, _)| *id == object)
                {
                    self.sculpt_pending = None;
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
        let org = doc.organisation();
        doc.objects()
            .iter()
            // Hidden objects don't render (Task "Project organisation"
            // step 2).
            .filter(|o| !org.is_hidden(o.id))
            .filter_map(|o| {
                let active = Some(o.id) == self.active;
                let render = if active
                    && let Some(stroke) = &self.sculpt_stroke
                    && stroke.object == o.id
                {
                    // A sculpt stroke in progress draws its own running
                    // mesh, not the last committed revision (Task 19).
                    stroke.render.clone()
                } else if active
                    && !self.shows_current_revision()
                    && let Some((object, r)) = &self.sculpt_pending
                    && *object == o.id
                {
                    // Just released: keep the stroke's last live mesh on
                    // screen (not the stale pre-stroke `displayed`, and no
                    // "Updating view…" over it) until the committed
                    // revision's own render mesh is ready to swap in,
                    // whole, in one frame (owner: no flicker on release).
                    r.clone()
                } else if active && let Some(r) = crate::hollow_tool::preview_render(self, o.id) {
                    // Hollow's preview: the object hollowed, in its place.
                    r
                } else if active {
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
                let color = object_color_for(self.object_colors, o, &render.mesh);
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
                    ghost: crate::split_flow::is_ghost(self, o.id),
                    color,
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

    /// Object `id`'s world bounding box, from vertices built on a worker
    /// (`None` until they're ready, or for an empty mesh). O(1).
    pub fn object_world_bounds(&self, id: ObjectId) -> Option<Aabb> {
        let o = self.session.document().object(id)?;
        let bb = self.object_bounds(o)??;
        Some(transformed_bounds(&bb, &model_matrix(&o.transform)))
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
            TreeItem::Group(g) => {
                // Selecting a group selects every member (Move, Hide,
                // Export and Delete then act on the whole group).
                let members = self.session.document().organisation().members(*g);
                self.picked = members;
                if let Some(&last) = self.picked.last() {
                    self.set_active(last);
                }
                self.clear_selection();
            }
        }
    }

    /// A row of the tree double-clicked (or Enter on it): select its item
    /// and frame it, with a short eased camera move
    /// ([`FRAME_MOVE_SECONDS`]) then a pulse of the selection
    /// ([`PULSE_SECONDS`]) so the eye finds it.
    pub fn tree_find(&mut self, item: &TreeItem) {
        self.tree_select(item);
        // A group has no single place to frame the camera on (its
        // members can sit anywhere); double-clicking it only selects.
        if matches!(item, TreeItem::Group(_)) {
            return;
        }
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
        self.hover = match item {
            TreeItem::Object(id) | TreeItem::Model(id) => Some((*id, OverlayTarget::Object)),
            TreeItem::Defect(id, d) => Some((*id, defect_target(d))),
            // A group has no single object to outline in the view.
            TreeItem::Group(_) => None,
        };
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
            // A group shows selected once every member is among the
            // objects picked together (and there's at least one).
            TreeItem::Group(g) => {
                let members = self.session.document().organisation().members(*g);
                !members.is_empty() && members.iter().all(|m| self.picked.contains(m))
            }
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
            // `tree_find` never starts a `Find` for a group.
            TreeItem::Group(_) => {
                self.find = None;
                return;
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
            sculpt_baseline: (self.active_tool == crate::sculpt_tool::SCULPT
                && self.sculpt_show_changes)
                .then(|| {
                    self.sculpt_baseline
                        .as_ref()
                        .filter(|(id, _)| *id == object)
                        .map(|(_, m)| m.clone())
                })
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
            | if self.active_tool == crate::sculpt_tool::SCULPT && self.sculpt_show_changes {
                u32::from(FaceStates::SCULPT_CHANGED_TIERS)
            } else {
                0
            }
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
                sculpt_baseline: None,
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
        // Hollow's preview: the parts left solid.
        if let Some(layer) = crate::hollow_tool::thin_layer(self) {
            layers.push(layer);
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

    /// Ask for every Solid/Hole pair's exact crossing curve
    /// (`mesh.intersections`) when roles, revisions or transforms changed
    /// (including a live Transform drag): the view keeps showing the
    /// previous curves until the new ones arrive (throttled to the latest
    /// request; a stale one in flight is cancelled by
    /// [`crate::analysis::LatestWorker`]).
    fn refresh_intersections(&mut self) {
        let doc = self.session.document();
        let mut objects: Vec<(ObjectId, Mesh, [u64; 12], bool)> = doc
            .objects()
            .iter()
            .filter_map(|o| {
                let mesh = o.mesh()?;
                let transform = self.shown_transform(o.id).unwrap_or(o.transform);
                Some((o.id, mesh.clone(), transform.map(f64::to_bits), o.is_hole()))
            })
            .collect();
        objects.sort_by_key(|(id, ..)| *id);
        let query = IntersectionQuery { objects };
        if self
            .intersection_query
            .as_ref()
            .is_some_and(|(q, _)| q.same_as(&query))
        {
            return;
        }
        let key = self.intersection_query.as_ref().map_or(1, |(_, k)| k + 1);
        let bvh_of = |id: ObjectId, mesh: &Mesh| -> Option<Arc<Bvh>> {
            self.bvhs
                .get(&id)
                .filter(|(m, _)| Mesh::ptr_eq(m, mesh))
                .map(|(_, b)| b.clone())
        };
        let mut pairs = Vec::new();
        for o in doc.objects() {
            if o.is_hole() {
                continue;
            }
            let Some(solid_mesh) = o.mesh() else { continue };
            let Some(solid_bvh) = bvh_of(o.id, solid_mesh) else {
                continue;
            };
            let solid_transform = self.shown_transform(o.id).unwrap_or(o.transform);
            for h in doc.objects() {
                if !h.is_hole() {
                    continue;
                }
                let Some(hole_mesh) = h.mesh() else { continue };
                let Some(hole_bvh) = bvh_of(h.id, hole_mesh) else {
                    continue;
                };
                let hole_transform = self.shown_transform(h.id).unwrap_or(h.transform);
                pairs.push(worker::IntersectionPair {
                    solid: o.id,
                    solid_mesh: solid_mesh.clone(),
                    solid_bvh: solid_bvh.clone(),
                    solid_transform,
                    hole: h.id,
                    hole_mesh: hole_mesh.clone(),
                    hole_bvh,
                    hole_transform,
                });
            }
        }
        self.intersection_query = Some((query, key));
        self.intersection_worker
            .submit(worker::IntersectionRequest { key, pairs });
    }

    /// Every overlapping Solid/Hole pair's crossing curve, for the 3D view
    /// (empty while none overlap, or none has a BVH yet).
    pub fn intersection_views(&self) -> Vec<crate::viewport::IntersectionView> {
        self.intersections
            .as_ref()
            .map(|(_, curves)| {
                curves
                    .iter()
                    .filter(|c| !c.lines.is_empty())
                    .map(|c| crate::viewport::IntersectionView {
                        lines: c.lines.clone(),
                    })
                    .collect()
            })
            // allow-swallow: None means no intersection query has run yet; an empty view list is correct, not an error.
            .unwrap_or_default()
    }

    /// The Alignment panel's hole preview, kept matching the Split plane
    /// and peg settings while Split is open with pegs on: cuts the active
    /// object on a worker ([`worker::peg_preview`]) and places holes,
    /// same as Apply will (minus creating anything). Cleared once pegs
    /// are off, or Split closes.
    fn refresh_peg_preview(&mut self) {
        let open = self.active_tool == crate::shape_tools::SPLIT;
        let Some(id) = self.active.filter(|_| open) else {
            self.peg_preview = None;
            self.peg_preview_asked = None;
            return;
        };
        let a = &self.alignment;
        if a.pegs == "none" {
            self.peg_preview = None;
            self.peg_preview_asked = None;
            return;
        }
        let Some(mesh) = self.active_mesh().cloned() else {
            return;
        };
        let transform = self
            .session
            .document()
            .object(id)
            .map(|o| o.transform)
            // allow-swallow: id came from self.active and is checked live above; a vanished object falls back to identity rather than panicking this frame.
            .unwrap_or_default();
        let key = crate::shape_tools::PegPreviewKey {
            object: id,
            transform,
            plane_point: self.split.point().to_array(),
            plane_normal: self.split.normal().to_array(),
            shape: a.peg_shape,
            diameter: a.hole_diameter,
            depth_above: a.hole_depth_above,
            depth_below: a.hole_depth_below,
            clearance: a.peg_clearance,
            allowance: a.length_allowance,
            count: a.peg_count,
            min_wall: a.min_wall,
        };
        if self.peg_preview_asked.as_ref() == Some(&key) {
            return;
        }
        self.peg_preview_asked = Some(key.clone());
        self.peg_preview_worker.submit(worker::PegPreviewRequest {
            object: id,
            mesh,
            transform: key.transform,
            plane_point: key.plane_point,
            plane_normal: key.plane_normal,
            shape: key.shape,
            diameter: key.diameter,
            depth_above: key.depth_above,
            depth_below: key.depth_below,
            clearance: key.clearance,
            allowance: key.allowance,
            count: key.count,
            min_wall: key.min_wall,
            unit: self.session.document().units(),
        });
    }

    /// What the view shows for the active object now (its render mesh),
    /// if it's this object's.
    pub(crate) fn active_render(&self) -> Option<Arc<RenderMesh>> {
        let (object, _, r) = self.displayed.as_ref()?;
        (Some(*object) == self.active).then(|| r.clone())
    }

    /// The Alignment panel's current hole preview, for the active
    /// object's current revision only.
    pub fn peg_preview(&self) -> Option<&worker::PegPreview> {
        let preview = self.peg_preview.as_deref()?;
        (Some(preview.object) == self.active
            && self
                .active_mesh()
                .is_some_and(|m| Mesh::ptr_eq(m, &preview.mesh)))
        .then_some(preview)
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
        if let Some(&(_, t)) = self.align.preview.iter().find(|(o, _)| *o == id) {
            return Some(t);
        }
        self.preview
            .filter(|(o, _)| *o == id)
            .map(|(_, t)| t)
            .or_else(|| crate::hinge_tool::swung(self, id))
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

    /// Whether a gizmo drag snaps right now: [`Self::gizmo_snap`] (the
    /// one shared preference every gizmo tool's own "Snap" checkbox
    /// shows and edits), inverted while Cmd/Ctrl is held (`invert`),
    /// exactly like [`Self::transform_snap`] always has — Split's plane
    /// and print-volume box, and any future gizmo tool, read this too
    /// instead of keeping their own switch (owner, 2026-11-14: "can't we
    /// disable snap?").
    pub(crate) fn gizmo_snap_on(&self, invert: bool) -> bool {
        self.gizmo_snap != invert
    }

    /// The line at the view's top-left while Transform or Split's gizmo
    /// is open, with the live grid step (MF-OBJ-002, "snap 1 mm") when
    /// snapping is on: the same zoom-adaptive, unit-aware step every
    /// gizmo drag now snaps to ([`transform_tool::snap_step`]).
    fn tool_hint(&self) -> Option<String> {
        let snap = self.gizmo_snap_on(false);
        if self.transform_tool().is_some() {
            let clause = self.snap_hint_clause(snap, self.transform_gizmo().map(|g| g.pivot));
            return Some(format!(
                "Transform · W move · E rotate · R scale{clause} · Cmd/Ctrl: snap off · Esc: done"
            ));
        }
        if self.active_tool == crate::shape_tools::SPLIT {
            let (gizmo, _) = crate::shape_tools::split_gizmo(self)?;
            let clause = self.snap_hint_clause(snap, Some(gizmo.pivot));
            return Some(format!("Split · drag the plane to move or tilt it{clause}"));
        }
        None
    }

    /// " · snap 1 mm", or "" when snapping is off or the step can't be
    /// worked out (no camera/viewport yet).
    fn snap_hint_clause(&self, snap_on: bool, pivot: Option<glam::DVec3>) -> String {
        if !snap_on {
            return String::new();
        }
        let (Some(cam), Some(rect), Some(pivot)) = (self.camera, self.viewport.last_rect, pivot)
        else {
            return String::new();
        };
        let view = (rect.width() as f64, rect.height() as f64);
        let (_, step_mm) =
            transform_tool::snap_step(&cam, pivot, view, self.session.document().units());
        format!(" · {}", transform_tool::snap_hint(step_mm))
    }

    /// The build plate, sized in the document's units. The plate's own
    /// physical size (`size_mm`) is fixed — it's a real print bed's
    /// dimensions, not something zoom should change — but its grid-line
    /// spacing is zoom-adaptive (consolidation plan C09), through the
    /// same [`mf_units::nice_step`] every gizmo snap now uses
    /// ([`transform_tool::snap_step`]): a fixed 10 mm grid reads as
    /// nothing at all zoomed far out on a large part, and as visual
    /// noise zoomed in close on a small one (ADR-0006 rule 3: "grid and
    /// cell sizes derive from the bounding box", "a literal length
    /// constant in document units is a bug"). `major_mm` is always 5
    /// minor steps, so the major/minor ratio stays the one the owner
    /// tuned (10 mm minor, 50 mm major) at every zoom. Falls back to the
    /// fixed default spacing without a live camera/viewport yet (the
    /// first frame, before either is set).
    fn plate(&self) -> mf_render::Plate {
        let mm = transform_tool::mm_per(self.session.document().units());
        let d = mf_render::Plate::default();
        let grid_mm = self
            .camera
            .zip(self.viewport.last_rect)
            .map(|(cam, rect)| {
                let view = (rect.width() as f64, rect.height() as f64);
                mf_render::adaptive_grid_mm(
                    &cam,
                    glam::DVec3::ZERO,
                    view,
                    mm,
                    transform_tool::MIN_SNAP_PX,
                )
            })
            .unwrap_or(d.grid_mm);
        mf_render::Plate {
            size_mm: d.size_mm.map(|v| v / mm),
            grid_mm: grid_mm / mm,
            major_mm: grid_mm * 5.0 / mm,
        }
    }

    /// The snapping a drag uses now: the shared [`Self::gizmo_snap`]
    /// switch, inverted while Cmd/Ctrl is held. The grid is a 1-2-5-mm
    /// step picked so it reads on screen at the current zoom
    /// (MF-OBJ-002, [`transform_tool::snap_step`] — a fixed
    /// 1-document-unit grid snapped every drag on a metre-scale document
    /// to zero); the plate snap needs the object's lowest point, found
    /// on a worker.
    fn transform_snap(&self, invert: bool, object: ObjectId, start: &[f64; 12]) -> Snap {
        let on = self.gizmo_snap_on(invert);
        if !on {
            return Snap {
                grid: None,
                angle_deg: None,
                scale_step: None,
                plate: None,
            };
        }
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
        let grid = self.transform_grab().map(|(g, cam, view)| {
            transform_tool::snap_step(&cam, g.pivot, view, self.session.document().units()).0
        });
        Snap {
            grid,
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
        if self.busy.is_some() || self.active != Some(object) || self.is_locked(object) {
            return;
        }
        if self.move_group(object, transform, &status) {
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

    /// A member of a linked group (a connector placed as several
    /// objects) moves with the rest of its group, so a peg and its hole
    /// stay aligned: every member gets the same world move as `object`,
    /// as one `object.move_group` step. False (nothing done) if `object`
    /// isn't in a group of more than one.
    fn move_group(&mut self, object: ObjectId, transform: [f64; 12], status: &str) -> bool {
        let doc = self.session.document();
        let Some(group) = doc
            .object(object)
            .and_then(|o| o.properties.get(mf_command::GROUP))
            .cloned()
        else {
            return false;
        };
        let members: Vec<(ObjectId, [f64; 12])> = doc
            .objects()
            .iter()
            .filter(|o| o.properties.get(mf_command::GROUP) == Some(&group))
            .map(|o| (o.id, o.transform))
            .collect();
        let Some(from) = doc.object(object).map(|o| o.transform) else {
            return false;
        };
        if members.len() < 2 {
            return false;
        }
        let affine = |t: &[f64; 12]| {
            glam::DAffine3::from_mat3_translation(
                crate::shape_panel::rotation(t),
                glam::DVec3::new(t[9], t[10], t[11]),
            )
        };
        let delta = affine(&transform) * affine(&from).inverse();
        let rows = |a: glam::DAffine3| {
            let m = a.matrix3;
            let t = a.translation;
            [
                m.x_axis.x, m.x_axis.y, m.x_axis.z, m.y_axis.x, m.y_axis.y, m.y_axis.z, m.z_axis.x,
                m.z_axis.y, m.z_axis.z, t.x, t.y, t.z,
            ]
        };
        let transforms: Vec<[f64; 12]> = members
            .iter()
            .map(|(m, t)| {
                if *m == object {
                    transform
                } else {
                    rows(delta * affine(t))
                }
            })
            .collect();
        let ids: Vec<ObjectId> = members.iter().map(|(m, _)| *m).collect();
        self.preview = Some((object, transform));
        self.pending_status = Some(status.to_owned());
        self.run_intent(
            crate::intent::Intent::op("object.move_group")
                .on(ids[0])
                .operands(ids[1..].iter().map(|&o| ("target".to_owned(), o, None)))
                .set_doc(
                    "transforms",
                    mf_operations::shapes::transforms_param(&transforms),
                ),
            "Move group".into(),
        );
        if self.busy.is_none() {
            self.preview = None;
            self.pending_status = None;
        }
        true
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
            "object.add_shape" => {
                let n = r["shapes"].as_array()?.len();
                Some(if n == 1 {
                    "Added an editable shape".to_owned()
                } else {
                    format!("Added {n} editable shapes as a linked group")
                })
            }
            "object.set_shape_params" => {
                let n = r["regenerated"].as_u64()?;
                Some(if n == 1 {
                    format!("Rebuilt {name}")
                } else {
                    format!("Rebuilt {name} and the rest of its group ({n} objects)")
                })
            }
            "object.attach" => Some(format!("Attached {} parts", r["attached"].as_u64()?)),
            "object.move_group" => Some(format!("Moved {name} with its group")),
            "mesh.combine" => {
                let n = rec.operands.len().max(1);
                Some(format!("Combined {n} object{} into {name}", plural_s(n)))
            }
            "mesh.make_solid" => {
                let cell = r["cell_size"].as_f64().filter(|&c| c > 0.0);
                Some(match cell {
                    Some(cell) => format!("Made {name} solid ({} mm cells)", measure(cell)),
                    None => format!("Made {name} solid"),
                })
            }
            "connector.join" => {
                let gap = r["measured_gap"].as_f64()?;
                Some(crate::t!(
                    "status.connector_join",
                    gap = crate::defects::measure(gap),
                    unit = crate::defects::unit_symbol(doc.units())
                ))
            }
            "mesh.hollow" => {
                let wall = r["wall_thickness"].as_f64()?;
                let unit = crate::defects::unit_symbol(doc.units());
                Some(if r["open_through"].is_null() {
                    crate::t!(
                        "hollow.status",
                        name = name,
                        wall = measure(wall),
                        unit = unit
                    )
                } else {
                    crate::t!(
                        "hollow.status_open",
                        name = name,
                        wall = measure(wall),
                        unit = unit
                    )
                })
            }
            "mesh.sculpt_stroke" => {
                let moved = r["moved_vertices"].as_u64()?;
                let brush = rec
                    .params
                    .get_choice("brush")
                    // allow-swallow: a status label only: the brush was validated when the stroke ran
                    .ok()
                    .and_then(mf_operations::sculpt::BrushKind::parse)
                    .map(|b| crate::i18n::tr(&format!("sculpt.brush.{}", b.word()), &[]))
                    .unwrap_or_else(|| "Sculpt".to_owned());
                // allow-swallow: strokes recorded before remeshing carry no count: none were split
                let splits = r["remesh_splits"].as_u64().unwrap_or_default();
                Some(if splits > 0 {
                    crate::i18n::tr_plural(
                        "status.sculpt.remeshed",
                        moved,
                        &[
                            ("brush", brush),
                            ("name", name.to_owned()),
                            ("splits", thousands(splits)),
                        ],
                    )
                } else {
                    crate::i18n::tr_plural(
                        "status.sculpt",
                        moved,
                        &[("brush", brush), ("name", name.to_owned())],
                    )
                })
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
        // A command queued behind a job that isn't a command (an export,
        // an import, a replay) starts once that job is over, not only
        // when some later command finishes (final review I2).
        self.drain_pending_invocation();
        crate::shape_panel::run_pending_reorient(self);
        self.run_pending_scale_to_fit();
        if self.busy.is_none()
            && let Some(pending) = self.pending_open_transform.take()
        {
            // `after_object_changes` (handled above, in the message
            // loop) already made the new object active if the command
            // committed; a refusal leaves `active` where it was, so
            // there's nothing of this placement to record or open.
            if let Some(new_object) = self.active {
                match pending {
                    PendingPlacement::Part { scan } if new_object != scan => {
                        self.parts.placed = Some(crate::parts_tool::PlacedPart {
                            part: new_object,
                            scan,
                        });
                        self.open_rail_tool(crate::transform_tool::ID);
                    }
                    PendingPlacement::Connector {
                        connector_id,
                        connector_params_json,
                        gap,
                        group,
                    } => {
                        self.connectors.placed = Some(crate::connector_tool::PlacedConnector {
                            object: new_object,
                            group,
                            connector_id,
                            connector_params_json,
                            gap,
                            side_a: None,
                            side_b: None,
                        });
                        self.open_rail_tool(crate::transform_tool::ID);
                    }
                    PendingPlacement::Part { .. } => {} // allow-swallow: active is still the scan, meaning add_part was refused; its own refusal card already says why
                }
            }
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
        let (delete, backspace) = if typing {
            (false, false)
        } else {
            ctx.input_mut(|i| {
                (
                    i.consume_key(egui::Modifiers::NONE, egui::Key::Delete),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::Backspace),
                )
            })
        };
        if (delete || backspace) && !ctx.egui_wants_keyboard_input() {
            // A tool that owns Delete or Backspace (Connect: the picked
            // connector) gets it first, and it never falls through to the
            // whole-object delete while that tool is open — even when it
            // has nothing picked (I1, owner 2026-11-11: deleting a
            // connector must never delete an object instead).
            let tool = self.active_tool_def();
            let key = if delete {
                egui::Key::Delete
            } else {
                egui::Key::Backspace
            };
            let owned = (tool.keys)(self)
                .iter()
                .find(|(k, _)| *k == key)
                .map(|&(_, k)| k);
            match owned {
                Some(k) => (tool.on_key)(self, k),
                None => self.delete_active_object(),
            }
        }
        // H: the selected objects' role, Solid or Hole.
        let role = !typing
            && !ctx.egui_wants_keyboard_input()
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::H));
        if role {
            let ids = self.selected_objects();
            self.toggle_role(&ids);
        }
        // F2: rename the active object in the Browser (Task "Project
        // organisation" step 1), same as the Browser row's "Rename…"
        // context menu item or the selection panel's name field.
        let f2 = !typing
            && !ctx.egui_wants_keyboard_input()
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F2));
        if f2
            && let Some(id) = self.active
            && let Some(name) = self.session.document().object(id).map(|o| o.name.clone())
        {
            self.renaming = Some((id, name));
        }
        // Cmd/Ctrl+G: group the selected objects (Task "Project
        // organisation" step 2).
        let group = !typing
            && !ctx.egui_wants_keyboard_input()
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::G));
        if group {
            self.group_selection();
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
        self.refresh_intersections();
        self.refresh_peg_preview();
        crate::hollow_tool::refresh_preview(self, &ctx);

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
        // measurements are worked out. A just-released stroke's own live
        // mesh (`sculpt_pending`) is current work, not an earlier
        // revision, so it never says "Updating view…" either.
        let sculpt_settling = self
            .sculpt_pending
            .as_ref()
            .is_some_and(|(id, _)| Some(*id) == self.active);
        let updating = self.is_analyzing() && !self.shows_current_revision() && !sculpt_settling;
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
                let hint = self.tool_hint();
                let split_preview = crate::shape_tools::split_preview(self);
                let shading = crate::shape_tools::shading(self);
                let intersections = self.intersection_views();
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
                        show_holes_through_solids: self.show_holes_through_solids,
                        intersections,
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
        crate::align_tool::view_overlay(self, &ctx);
        crate::sculpt_tool::cursor_overlay(self, &ctx);
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

/// View ▸ Colour objects (UI plan §11): `o`'s palette shade, or `None`
/// when the toggle is off (Clay), `o` is a Hole (it always keeps its
/// hatch instead) or `mesh` carries its own 3MF material or colour (which
/// always overrides the palette). Linked parts (a connector's peg and
/// hole, a hinge's leaves and pin: `mf_command::GROUP`) share a hue and
/// vary only in shade, so a group reads as one related family.
fn object_color_for(on: bool, o: &mf_command::Object, mesh: &Mesh) -> Option<[f32; 4]> {
    if !on || o.is_hole() || mesh.has_materials() {
        return None;
    }
    let group = o.properties.get(mf_command::GROUP).map(String::as_str);
    Some(theme::rgba(theme::object_color(o.id.0, group)))
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
        "mesh.split_plane" => {
            let (parts, joints) = (len("parts") as u64, len("joints") as u64);
            let mut text = format!("Cut into {} part{}", thousands(parts), s(parts));
            if joints > 0 {
                text.push_str(&format!(
                    "; {} joint{} ready for connectors (Split, step 3)",
                    thousands(joints),
                    s(joints)
                ));
            }
            text
        }
        "mesh.split_box" => {
            let (pieces, joints) = (len("pieces") as u64, len("joints") as u64);
            format!(
                "Cut into {} piece{} along the print volume; {} joint{} ready for connectors \
                 (Split, step 3)",
                thousands(pieces),
                s(pieces),
                thousands(joints),
                s(joints)
            )
        }
        "split.place_connectors" => {
            let (n, dowels) = (num("connectors"), num("dowels"));
            let mut text = format!("Added {} connector{}", thousands(n), s(n));
            if dowels > 0 {
                text.push_str(&format!(
                    ", with {} loose dowel{} on the plate",
                    thousands(dowels),
                    s(dowels)
                ));
            }
            for w in report["warnings"].as_array().into_iter().flatten() {
                if let Some(w) = w.as_str() {
                    text.push_str(". ");
                    text.push_str(w);
                }
            }
            text
        }
        "mesh.boolean" => {
            let op = match report["operation"].as_str() {
                Some("union") => "Combine",
                Some("subtract") => "Subtract",
                Some("intersect") => "Intersect",
                _ => "Boolean",
            };
            crate::tp!("status.boolean", num("created_faces"), op = op)
        }
        "mesh.delete_faces" => crate::tp!("status.delete_faces", num("removed_faces")),
        // "connector.join", "mesh.combine" and "mesh.make_solid" have
        // their own plain words in `object_report` (they need the
        // document's units or the object's name, not in the report).
        "object.add_part" => crate::tp!("status.part_added", num("faces")),
        "object.arrange_on_plate" => {
            if report["off_plate"].as_bool() == Some(true) {
                crate::t!("status.placed_beside_plate")
            } else {
                crate::t!("status.placed_on_plate")
            }
        }
        // "mesh.sculpt_stroke" has its own plain words in `object_report`
        // (it needs the brush and object name, which aren't in the
        // engine's own report: see the module docs there).
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

/// A history step in plain words: "Import part", "Fill hole", "Remove
/// piece", "Split scan into 2 pieces", "Moved part by 12, 0, 0 mm" (I5,
/// owner 2026-11-11: the shape-making operations name their object and
/// what happened, not their raw parameter list).
fn step_label(session: &Session, node: &mf_command::RevisionNode) -> String {
    if let Some(import) = &node.import {
        return format!("Import {}", import.name);
    }
    node.record
        .as_ref()
        .and_then(|rec| {
            history_label(session, node, rec)
                .or_else(|| {
                    let target = rec.selection.as_ref().map(|s| (s.kind, s.ids.len()));
                    operation_label(&rec.op_id, target).or_else(|| object_step_label(&rec.op_id))
                })
                .or_else(|| catalogue_history_label(node, rec))
        })
        // allow-swallow: a step with no plain words keeps the spec's own
        // title and the object's name (never raw parameters: see
        // Session::commit's `label`, which is this same safe fallback).
        .unwrap_or_else(|| node.label.clone())
}

/// The history row for every operation that has neither a bespoke
/// [`history_label`] branch nor a plain name from [`operation_label`] /
/// [`object_step_label`] (most whole-object and object-graph operations):
/// looks up the spec's `history_label` message id and fills it from the
/// command's own words — the object's name, counts, choices — never a
/// raw parameter value or an id list (the owner's recurring bug).
/// `None` if the spec or its message id is missing (a build defect,
/// caught by `every_built_in_spec_has_a_history_label`), so the caller
/// falls through to the safe generic fallback instead of panicking here.
fn catalogue_history_label(
    node: &mf_command::RevisionNode,
    rec: &mf_command::CommandRecord,
) -> Option<String> {
    let spec = mf_spec::builtin::get(&rec.op_id)?;
    if !crate::i18n::has_message_or_plural(&spec.history_label) {
        return None;
    }
    let name = result_name(node, rec.target);
    Some(match rec.op_id.as_str() {
        "mesh.delete_faces" => {
            let n = rec.selection.as_ref().map_or(0, |s| s.ids.len());
            crate::tp!("history.label.mesh.delete_faces", n, name = name)
        }
        "mesh.fill_holes" => {
            let n = rec.selection.as_ref().map_or(1, |s| s.ids.len().max(1));
            crate::tp!("history.label.mesh.fill_holes", n, name = name)
        }
        "mesh.remove_small_components" => {
            let n = rec.selection.as_ref().map_or(1, |s| s.ids.len().max(1));
            crate::tp!("history.label.mesh.remove_small_components", n, name = name)
        }
        "mesh.compact" => crate::t!("history.label.mesh.compact", name = name),
        "mesh.inspect" => crate::t!("history.label.mesh.inspect", name = name),
        "mesh.intersections" => crate::t!("history.label.mesh.intersections", name = name),
        "mesh.split_region" => crate::t!("history.label.mesh.split_region", name = name),
        "mesh.boolean" => {
            // allow-swallow: a label only: an unreadable operation choice shows as a generic Combine
            let op = match rec.params.get_choice("operation").ok() {
                Some("union") => "Combine",
                Some("subtract") => "Subtract",
                Some("intersect") => "Intersect",
                _ => "Combine",
            };
            crate::t!("history.label.mesh.boolean", op = op, name = name)
        }
        "mesh.hollow" => {
            // allow-swallow: a label only: an unreadable wall value falls back to the spec default
            let wall = rec.params.get_float("wall_thickness").unwrap_or(2.0);
            let open = rec.params.get_bool("open_through").unwrap_or(false);
            // `wall` is already a document-unit value (ADR-0006 rule 2):
            // on a metres document the spec's own "mm" was wrongly taken
            // as the shown unit (consolidation plan C11).
            let wall_text = mf_units::format_length(wall, node.document.units());
            if open {
                crate::t!(
                    "history.label.mesh.hollow.open",
                    name = name,
                    wall = wall_text
                )
            } else {
                crate::t!("history.label.mesh.hollow", name = name, wall = wall_text)
            }
        }
        "mesh.sculpt_stroke" => {
            let brush = rec
                .params
                .get_choice("brush")
                // allow-swallow: a label only: an unreadable brush choice shows as a generic stroke
                .ok()
                .and_then(mf_operations::sculpt::BrushKind::parse)
                .map(|b| crate::i18n::tr(&format!("sculpt.brush.{}", b.word()), &[]))
                .unwrap_or_else(|| "Sculpt".to_owned());
            crate::t!(
                "history.label.mesh.sculpt_stroke",
                name = name,
                brush = brush
            )
        }
        "connector.join" => crate::t!("history.label.connector.join", name = name),
        "object.add_part" => crate::t!("history.label.object.add_part", name = name),
        "object.add_shape" => {
            let n = rec.outputs.iter().filter(|o| o.created).count().max(1);
            crate::tp!("history.label.object.add_shape", n, name = name)
        }
        "object.align" => {
            let n = rec.operand_slot("target").count().max(1);
            crate::tp!("history.label.object.align", n)
        }
        "object.arrange_on_plate" => {
            crate::t!("history.label.object.arrange_on_plate", name = name)
        }
        "object.attach" => {
            let n = rec.operand_slot("members").count().max(1);
            crate::tp!("history.label.object.attach", n, name = name)
        }
        "object.delete" => crate::t!("history.label.object.delete", name = name),
        "object.drop_to_plate" => crate::t!("history.label.object.drop_to_plate", name = name),
        "object.move_group" => crate::t!("history.label.object.move_group", name = name),
        "object.set_shape_params" => {
            crate::t!("history.label.object.set_shape_params", name = name)
        }
        _ => return None,
    })
}

/// The object a command's output names, as the history row should: its
/// name after the command (combine and make_solid keep the target's
/// name; place_connectors doesn't rename anything).
fn result_name(node: &mf_command::RevisionNode, id: ObjectId) -> String {
    node.document
        .object(id)
        .map_or_else(|| "the object".to_owned(), |o| o.name.clone())
}

/// The object's name before the command ran: for a step that renames its
/// target (split's target becomes "<name> (top)"), the row should still
/// read "Split scan into 2 pieces", not "Split scan (top) into 2 pieces"
/// — "(top)" only makes sense once you know it's one of the two results.
fn before_name(session: &Session, rec: &mf_command::CommandRecord, id: ObjectId) -> String {
    session
        .node(rec.input_revision)
        .and_then(|before| before.document.object(id))
        .map_or_else(|| "the object".to_owned(), |o| o.name.clone())
}

/// The shape-making operations' history label, with the object's own
/// name and what happened to it (I5). `None` leaves `step_label` to its
/// generic fallbacks (a selection-based tool, or the raw label).
fn history_label(
    session: &Session,
    node: &mf_command::RevisionNode,
    rec: &mf_command::CommandRecord,
) -> Option<String> {
    match rec.op_id.as_str() {
        "object.rename" => {
            let id = rec.target;
            let before = before_name(session, rec, id);
            let after = result_name(node, id);
            Some(format!("Renamed {before} to {after}"))
        }
        "mesh.split_plane" | "mesh.split_box" => {
            let name = before_name(session, rec, rec.target);
            let pieces = rec.outputs.iter().filter(|o| o.created).count() + 1;
            Some(format!(
                "Split {name} into {} piece{}",
                pieces,
                plural_s(pieces)
            ))
        }
        "mesh.make_solid" => {
            let name = result_name(node, rec.target);
            // allow-swallow: a label only: no cell size reads as the automatic one
            let cell = rec.params.get_float("cell_size").ok().filter(|&c| c > 0.0);
            Some(match cell {
                Some(cell) => format!("Made {name} solid ({} mm cells)", measure(cell)),
                None => format!("Made {name} solid"),
            })
        }
        "mesh.combine" => {
            let name = result_name(node, rec.target);
            let n = rec.operands.len().max(1);
            Some(format!("Combined {n} object{} into {name}", plural_s(n)))
        }
        "split.place_connectors" => {
            let name = result_name(node, rec.target);
            let placements: Option<Vec<mf_operations::Placement>> = rec
                .params
                .get_text("connectors")
                // allow-swallow: a label only: unreadable placements get the generic wording
                .ok()
                // allow-swallow: a label only: unreadable placements get the generic wording
                .and_then(|s| serde_json::from_str(s).ok());
            Some(match placements {
                Some(placements) if !placements.is_empty() => {
                    let n = placements.len();
                    let first = &placements[0].connector;
                    if placements.iter().all(|p| &p.connector == first) {
                        let kind = crate::split_flow::kind_label_plural(first);
                        format!("Added {n} {kind} to {name}'s joint")
                    } else {
                        format!("Added {n} connector{} to {name}'s joint", plural_s(n))
                    }
                }
                _ => format!("Added connectors to {name}'s joint"),
            })
        }
        "object.set_transform" => {
            let id = rec.target;
            let before = session.node(rec.input_revision)?;
            let from = before.document.object(id)?.transform;
            let to = node.document.object(id)?.transform;
            if from == to {
                return None;
            }
            let name = result_name(node, id);
            let mesh = node.document.object(id)?.mesh()?;
            let centre = mesh
                .bounding_box()
                .map_or(glam::DVec3::ZERO, |b| glam::DVec3::from(b.center()));
            Some(transform_tool::describe(
                &name,
                node.document.units(),
                &from,
                &to,
                centre,
            ))
        }
        _ => None,
    }
}

fn plural_s(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Every parameter `rec` ran with that applies (not hidden by another
/// parameter's value) and isn't left at the spec's default, as
/// "name: value" lines, for the history row's hover tooltip (I5).
/// Parameters the label itself already states (the transform, the
/// connector list) are left out: they'd only repeat it.
fn history_details(node: &mf_command::RevisionNode) -> String {
    node.record.as_ref().map_or_else(String::new, |rec| {
        history_details_of(&rec.op_id, &rec.params, node.document.units())
    })
}

/// [`history_details`], from an op id and its parameters directly (so it
/// can be tested without a whole revision). `unit` is the *document's*
/// unit (consolidation plan C11): every recorded length parameter is
/// already a document-unit value (`Session::prepare`'s one conversion
/// boundary, ADR-0006 rule 2), never the spec's own declared unit
/// (almost always mm, D16) — labelling a 0.002 doc-unit value "mm" on a
/// metres document was the owner's exact bug ("History Details shows
/// '0.00200 mm' for a 2 mm wall on a metres doc").
fn history_details_of(op_id: &str, params: &mf_spec::Params, unit: mf_io::threemf::Unit) -> String {
    let Some(spec) = mf_spec::builtin::get(op_id) else {
        return String::new();
    };
    let hidden_params: &[&str] = match op_id {
        // allow-swallow: only hides fields: without a pegs choice every field shows
        "mesh.split_plane" if params.get_choice("pegs").ok() == Some("none") => &[
            "peg_shape",
            "hole_profile",
            "hole_diameter",
            "hole_depth_above",
            "hole_depth_below",
            "peg_count",
            "peg_clearance",
            "length_allowance",
            "fit_clearance",
            "machine_tolerance",
            "min_wall",
            "plate_width",
            "plate_depth",
            "plate_spacing",
        ],
        "object.set_transform" => &["transform"],
        "split.place_connectors" => &["connectors"],
        "object.rename" => &["name"],
        _ => &[],
    };
    let mut lines: Vec<String> = spec
        .parameters
        .iter()
        .filter(|p| !hidden_params.contains(&p.name.as_str()))
        .filter_map(|p| {
            let value = params.0.get(&p.name)?;
            (*value != p.kind.default_value()).then(|| {
                format!(
                    "{}: {}",
                    param_label(op_id, &p.name),
                    summarize_value(value, p.unit, unit)
                )
            })
        })
        .collect();
    lines.sort();
    lines.join("\n")
}

/// A parameter's value in one line of plain words, for the history row's
/// Details section (I5, owner 2026-11-13: a raw dump — a `faces` list of
/// hundreds of ids, an `affine`'s 12 numbers, a bare float with no unit —
/// is not for people). Never longer than one line.
///
/// `value` is always already in *document* units when `param_unit` is
/// [`mf_spec::ParamUnit::Length`] (`Session::prepare`'s one conversion
/// boundary, ADR-0006 rule 2: a recorded length parameter is never left
/// in the spec's own declared unit, almost always mm, D16). `doc_unit`
/// is the document's unit, the only one ever shown; it goes through
/// [`mf_units::format_length`] rather than a literal `"mm"` suffix, the
/// owner's exact bug (consolidation plan C11: "History Details shows
/// '0.00200 mm' for a 2 mm wall on a metres doc").
fn summarize_value(
    value: &ParamValue,
    param_unit: mf_spec::ParamUnit,
    doc_unit: mf_io::threemf::Unit,
) -> String {
    match value {
        ParamValue::Faces(l) => crate::defects::count(l.faces.len(), "face"),
        ParamValue::Samples(s) => crate::defects::count(s.len(), "dab"),
        ParamValue::Affine(m) => summarize_affine(m, doc_unit),
        ParamValue::Float(f) if matches!(param_unit, mf_spec::ParamUnit::Length(_)) => {
            mf_units::format_length(*f, doc_unit)
        }
        ParamValue::Float(f) if matches!(param_unit, mf_spec::ParamUnit::Percent) => {
            format!("{}%", crate::defects::measure(*f))
        }
        ParamValue::Vec3(v) if matches!(param_unit, mf_spec::ParamUnit::Length(_)) => format!(
            "{}, {}, {}",
            mf_units::format_length(v[0], doc_unit),
            mf_units::format_length(v[1], doc_unit),
            mf_units::format_length(v[2], doc_unit)
        ),
        other => other.to_string(),
    }
}

/// An absolute transform in plain words: its move, rotation and scale,
/// each left out when it's the identity (a mesh.combine's default
/// placement, say "moved 10, 0, 0 mm" with no rotation or scale said).
/// A sheared transform (no clean rotate/scale split) just says so.
/// `doc_unit` is the document's unit: the translation is a recorded,
/// already-doc-unit value, shown through [`mf_units::format_length`]
/// rather than a literal `"mm"` (consolidation plan C11).
fn summarize_affine(m: &[f64; 12], doc_unit: mf_io::threemf::Unit) -> String {
    let fmt3 = |v: glam::DVec3| {
        format!(
            "{}, {}, {}",
            mf_units::significant_figures(v.x, 3),
            mf_units::significant_figures(v.y, 3),
            mf_units::significant_figures(v.z, 3)
        )
    };
    match mf_render::decompose(m) {
        mf_render::Decomposed::Trs {
            translate,
            rotate_deg_xyz,
            scale,
        } => {
            let mut parts = Vec::new();
            if translate.length() > 1e-9 {
                parts.push(format!("moved {} {}", fmt3(translate), doc_unit.symbol()));
            }
            if rotate_deg_xyz.length() > 1e-9 {
                parts.push(format!("rotated {}°", fmt3(rotate_deg_xyz)));
            }
            if (scale - glam::DVec3::ONE).length() > 1e-9 {
                parts.push(format!("scaled {}", fmt3(scale)));
            }
            if parts.is_empty() {
                "unchanged placement".to_owned()
            } else {
                parts.join(", ")
            }
        }
        mf_render::Decomposed::Sheared => "a sheared placement".to_owned(),
    }
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
#[allow(clippy::unwrap_used)] // tests may panic on failure
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

    /// Every operation the status line commonly shows gets plain words,
    /// never the report's raw JSON (the owner's bug: `{"created_faces":
    /// 0,"dabs":5,...}` showing after a sculpt stroke). `mesh.combine`,
    /// `mesh.make_solid`, `connector.join` and `mesh.sculpt_stroke` need
    /// more of the app's state (the object's name, the document's units)
    /// than `plain_report` sees, so they're covered by `object_report`
    /// instead; this test covers what reaches `plain_report` itself.
    #[test]
    fn no_raw_json_reaches_the_status_line() {
        let boolean = serde_json::json!({
            "operation": "union",
            "created_faces": 42,
            "removed_faces": 3,
        });
        assert_eq!(
            plain_report("mesh.boolean", &boolean),
            "Combine added 42 triangles"
        );
        let deleted = serde_json::json!({"removed_faces": 1, "removed_vertices": 3});
        assert_eq!(
            plain_report("mesh.delete_faces", &deleted),
            "Deleted 1 triangle"
        );
        let part = serde_json::json!({"part": "bolt-m4", "part_hash": "abc", "faces": 128});
        assert_eq!(
            plain_report("object.add_part", &part),
            "Added the part: 128 triangles"
        );
        let on_plate = serde_json::json!({"transform": vec![0.0; 12], "off_plate": false});
        assert_eq!(
            plain_report("object.arrange_on_plate", &on_plate),
            "Arranged on the plate"
        );
        let beside = serde_json::json!({"transform": vec![0.0; 12], "off_plate": true});
        assert_eq!(
            plain_report("object.arrange_on_plate", &beside),
            "The plate is full; placed beside it"
        );
        for (op, report) in [
            ("mesh.boolean", &boolean),
            ("mesh.delete_faces", &deleted),
            ("object.add_part", &part),
            ("object.arrange_on_plate", &on_plate),
        ] {
            let text = plain_report(op, report);
            assert!(
                !text.starts_with('{'),
                "{op}'s status text is still raw JSON: {text}"
            );
        }
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

    /// A split's history details: with pegs none (the default), every
    /// pegs-only parameter is hidden even though the fit profile left
    /// some of them off their spec default (I5, owner 2026-11-11).
    #[test]
    fn split_details_hide_pegs_only_parameters_when_pegs_is_none() {
        let spec = mf_spec::builtin::get("mesh.split_plane").unwrap();
        let raw: BTreeMap<String, String> = spec
            .parameters
            .iter()
            .map(|p| (p.name.clone(), p.kind.default_value().to_string()))
            .collect();
        let params = spec.validate(&raw).unwrap();
        let details = history_details_of(
            "mesh.split_plane",
            &params,
            mf_io::threemf::Unit::Millimeter,
        );
        assert_eq!(details, "", "every parameter is at its default: {details}");

        let mut raw = raw;
        raw.insert("hole_diameter".into(), "7.5".into());
        raw.insert("cut_gap".into(), "0.4".into());
        let params = spec.validate(&raw).unwrap();
        let details = history_details_of(
            "mesh.split_plane",
            &params,
            mf_io::threemf::Unit::Millimeter,
        );
        assert!(
            !details.contains("Hole diameter"),
            "pegs is none: hole_diameter doesn't apply: {details}"
        );
        assert!(details.contains("Cut gap: 0.4"), "{details}");
    }

    /// With pegs on, the alignment parameters that differ from their
    /// defaults show (they apply this time).
    #[test]
    fn split_details_show_pegs_parameters_when_pegs_is_set() {
        let spec = mf_spec::builtin::get("mesh.split_plane").unwrap();
        let mut raw: BTreeMap<String, String> = spec
            .parameters
            .iter()
            .map(|p| (p.name.clone(), p.kind.default_value().to_string()))
            .collect();
        raw.insert("pegs".into(), "dowels".into());
        raw.insert("hole_diameter".into(), "7.5".into());
        let params = spec.validate(&raw).unwrap();
        let details = history_details_of(
            "mesh.split_plane",
            &params,
            mf_io::threemf::Unit::Millimeter,
        );
        assert!(details.contains("Hole diameter: 7.5"), "{details}");
        assert!(details.contains("Pegs: dowels"), "{details}");
    }

    /// A connector list and a transform are left out of the details: the
    /// label already states them, and the transform's 12 numbers are not
    /// plain words anyway.
    #[test]
    fn transform_and_connectors_are_left_out_of_the_details() {
        let spec = mf_spec::builtin::get("object.set_transform").unwrap();
        let raw: BTreeMap<String, String> = spec
            .parameters
            .iter()
            .map(|p| (p.name.clone(), p.kind.default_value().to_string()))
            .collect();
        let mut raw = raw;
        raw.insert("transform".into(), "1,0,0,0,1,0,0,0,1,12,0,0".into());
        let params = spec.validate(&raw).unwrap();
        assert_eq!(
            history_details_of(
                "object.set_transform",
                &params,
                mf_io::threemf::Unit::Millimeter
            ),
            ""
        );

        let spec = mf_spec::builtin::get("split.place_connectors").unwrap();
        let raw: BTreeMap<String, String> = spec
            .parameters
            .iter()
            .map(|p| (p.name.clone(), p.kind.default_value().to_string()))
            .collect();
        let params = spec.validate(&raw).unwrap();
        assert!(
            !history_details_of(
                "split.place_connectors",
                &params,
                mf_io::threemf::Unit::Millimeter
            )
            .contains("connectors")
        );
    }

    /// Every registered operation spec's `history_label` resolves in the
    /// English catalogue (owner 2026-11-13: the History panel showed a
    /// raw parameter dump for the third time; a spec whose label typos
    /// its message id, or whose id was never added to the catalogue,
    /// must fail a test, not ship silently to `step_label`'s safe
    /// fallback). `mf_spec::builtin::specs()` already requires a
    /// non-empty `history_label` (schema `minLength: 1`,
    /// `every_built_in_spec_has_a_history_label` in mf-spec); this is
    /// the half only mf-desktop can check, since the catalogue lives
    /// here.
    #[test]
    fn every_built_in_spec_has_a_catalogued_history_label() {
        for spec in mf_spec::builtin::specs() {
            assert!(
                crate::i18n::has_message_or_plural(&spec.history_label),
                "{}'s history_label {:?} isn't in the i18n catalogue",
                spec.id,
                spec.history_label
            );
        }
    }

    /// No history label's catalogue text, rendered with representative
    /// words for every placeholder it declares, ever shows a raw
    /// `key=value` pair or a numeric id list — the owner's recurring bug
    /// (`mesh.hollow (drain_holes=none, ..., open_through_faces=37048,
    /// 37049, ...)`). Placeholders are filled with plain words (never a
    /// number or a list), so a template that still prints a parameter
    /// name or an id list fails here rather than in the shipped app.
    #[test]
    fn every_history_label_renders_without_raw_params_or_id_lists() {
        let sample_args: &[(&str, String)] = &[
            ("name", "widget".to_owned()),
            ("n", "3".to_owned()),
            ("wall", "2 mm".to_owned()),
            ("op", "Combined".to_owned()),
            ("brush", "Inflate".to_owned()),
        ];
        let never_raw = |id: &str, text: &str| {
            assert!(
                !text.contains('{') && !text.contains('}'),
                "{id}: unfilled placeholder: {text:?}"
            );
            assert!(
                !text.contains('='),
                "{id}: looks like a raw key=value: {text:?}"
            );
            // A numeric id list (hundreds of face ids) reads as several
            // digit groups joined by ", " or ",": two or more comma- or
            // space-separated runs of digits in a row.
            let digit_groups = text
                .split([',', ' '])
                .filter(|w| !w.is_empty() && w.chars().all(|c| c.is_ascii_digit()))
                .count();
            assert!(digit_groups < 2, "{id}: looks like an id list: {text:?}");
        };
        for spec in mf_spec::builtin::specs() {
            let id = spec.history_label.as_str();
            if crate::i18n::has(id) {
                never_raw(id, &crate::i18n::tr(id, sample_args));
            } else {
                // A plural pair: both forms render cleanly, for n = 1 and
                // n > 1 (English picks .other for the latter).
                never_raw(id, &crate::i18n::tr_plural(id, 1, sample_args));
                never_raw(id, &crate::i18n::tr_plural(id, 3, sample_args));
            }
        }
    }

    /// The owner's literal report: a sphere split near its equator, then
    /// Hollow with `open_through` opening through the cap — hundreds of
    /// input-face seeds — must render as "Hollowed ... walls, open at
    /// the cut face", never with `drain_holes=`/`open_through_faces=` or
    /// the raw id list, through the real command layer (not a synthetic
    /// `Params`), reproducing the History panel row exactly as shipped.
    #[test]
    fn hollow_with_hundreds_of_open_through_faces_renders_a_plain_label() {
        let dir = tempfile::tempdir().unwrap();
        let model = mf_io::threemf::Model {
            metadata: Default::default(),
            unit: mf_io::threemf::Unit::Millimeter,
            objects: vec![mf_io::threemf::ModelObject {
                metadata: Default::default(),
                name: "widget".into(),
                mesh: mf_fixtures::scaled(&mf_fixtures::uv_sphere(48, 96), 20.0),
                transform: mf_io::threemf::IDENTITY,
            }],
        };
        let path = dir.path().join("doc.3mf");
        mf_io::write_model_path(&model, &path).unwrap();
        let mut session = Session::new();
        let (ids, _) = session.import_model(mf_io::read_model(&path).unwrap());
        let id = ids[0];

        let cut = session
            .execute(
                crate::intent::Intent::op("mesh.split_plane")
                    .on(id)
                    .set_point("plane_point", [0.0, 0.0, 2.0], mf_units::Unit::Millimeter)
                    .param("plane_normal", "0,0,1")
                    .param("keep", "above")
                    .param("cap", true)
                    .build()
                    .unwrap(),
            )
            .unwrap();
        let mf_command::CommitOutcome::NewRevision { report } = cut else {
            panic!("expected a new revision");
        };
        let cap_faces: Vec<String> = report["parts"][0]["cap_faces"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap().to_string())
            .collect();
        assert!(
            cap_faces.len() > 100,
            "expected hundreds of cap faces, got {}",
            cap_faces.len()
        );
        let faces_param = cap_faces.join(",");

        session
            .execute(
                crate::intent::Intent::op("mesh.hollow")
                    .on(id)
                    .set(
                        "wall_thickness",
                        mf_units::Length::new(2.0, mf_units::Unit::Millimeter),
                    )
                    .set(
                        "inner_detail",
                        mf_units::Length::new(0.5, mf_units::Unit::Millimeter),
                    )
                    .param("open_through", true)
                    .param("open_through_faces", &faces_param)
                    .build()
                    .unwrap(),
            )
            .unwrap();

        let node = session.node(session.current()).unwrap();
        let label = step_label(&session, node);
        // `keep: above` renames the kept object "widget (top)" (D5): the
        // label correctly names the object as it is now, not its
        // pre-split name.
        assert!(label.starts_with("Hollowed widget (top): "), "{label:?}");
        assert!(label.contains("open at the cut face"), "{label:?}");
        assert!(!label.contains("drain_holes"), "{label:?}");
        assert!(!label.contains("open_through_faces"), "{label:?}");
        assert!(!label.contains("mesh.hollow"), "{label:?}");
        assert!(!label.contains('='), "{label:?}");
        // The cap's face ids never leak into the label: no run of 3+
        // comma/space-separated digit groups (a lone "2" from "2 mm" is
        // fine).
        let digit_groups = label
            .split([',', ' '])
            .filter(|w| !w.is_empty() && w.chars().all(|c| c.is_ascii_digit()))
            .count();
        assert!(digit_groups < 2, "{label:?}");
    }
}
