//! The Split tool (owner 2026-11-11): cutting only. "Define the cut,
//! then Cut" — no step tabs, no connectors here. Each cut's faces
//! become a [`Joint`], shared with the Connect tool
//! (`crate::connect_tool`), which the status line's "Add connectors to
//! these joints →" link opens on the new joints.
//!
//! *Slice*: one plane, placed with the gizmo and the Horizontal/
//! Vertical/tilt presets. *Print volume*: a translucent box (about 25%
//! opaque, edges drawn) moved with the gizmo and resized with a handle
//! on each face, or typed W×D×H, from the printer presets (the Machine
//! profiles' own volumes first, then `mf_parts::PRINT_VOLUME_PRESETS`).
//! The box's faces are the cut planes; the panel lists the pieces it
//! would make with their sizes, and warns before Cut when it misses or
//! already holds the model. No automatic suggestions.
//!
//! `mesh.split_plane` or `mesh.split_box`: every cut capped flat, one
//! undo step. Each cut face pair the command reports becomes a
//! [`Joint`] ([`after_commit`]); the status line then reports the piece
//! count and the panel offers the link into Connect.
//!
//! The [`Joint`] bookkeeping, its background geometry read, live checks
//! and previews are shared state (`app.split_flow`): the Connect tool
//! reads and mutates it through the `pub` items here, since a joint
//! outlives the Split tool that made it.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, RichText};
use glam::DVec3;
use mf_command::{Invocation, ObjectId, RevisionId};
use mf_geometry::{Aabb, Bvh, Mesh};
use mf_operations::{Check, JointFrame, JointSide, Placement};
use mf_render::{Gizmo, GizmoMode, Handle, Snap};

use crate::MeshFixerApp;
use crate::shell::{Claims, Input};
use crate::widgets::{self, Kind};
use crate::{icons, t, theme};

// ---------------------------------------------------------------------
// State.
// ---------------------------------------------------------------------

/// How the cut is defined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CutMode {
    #[default]
    Slice,
    Volume,
}

/// The print-volume box: world axis-aligned, document units.
#[derive(Debug, Clone)]
pub struct VolumeBox {
    pub center: DVec3,
    pub size: DVec3,
    /// The preset it came from ("Custom" once typed or dragged).
    pub preset: String,
    /// The selection it was last placed for (a new selection places it
    /// again, centred on it, resting on the plate).
    pub placed_for: Option<Vec<ObjectId>>,
    /// A gizmo drag moving it: (drag, start placement).
    pub drag: Option<(mf_render::Drag, [f64; 12])>,
    pub hover: Option<Handle>,
    /// A face handle drag: which face (`mf_operations::BOX_FACES`' order),
    /// the box at the start, and where on the face's axis it was grabbed.
    pub face_drag: Option<(usize, DVec3, DVec3, f64)>,
    pub hover_face: Option<usize>,
}

impl Default for VolumeBox {
    fn default() -> VolumeBox {
        VolumeBox {
            center: DVec3::new(0.0, 0.0, 128.0),
            size: DVec3::splat(256.0),
            preset: String::new(),
            placed_for: None,
            drag: None,
            hover: None,
            face_drag: None,
            hover_face: None,
        }
    }
}

impl VolumeBox {
    pub fn min(&self) -> DVec3 {
        self.center - self.size / 2.0
    }

    pub fn max(&self) -> DVec3 {
        self.center + self.size / 2.0
    }

    /// Face `k`'s centre and outward normal (`BOX_FACES`' order).
    fn face(&self, k: usize) -> (DVec3, DVec3) {
        let (axis, sign, _) = mf_operations::BOX_FACES[k];
        let mut n = DVec3::ZERO;
        n[axis] = sign;
        (self.center + n * self.size[axis] / 2.0, n)
    }
}

/// One cut face pair a cut recorded: pieces `a` and `b`, the cut's frame
/// (normal from `a` into `b`), which piece is drawn see-through, the
/// connectors placed on it (Connect's own state), and (once applied) the
/// revisions before and after `split.place_connectors`.
#[derive(Debug, Clone)]
pub struct Joint {
    pub a: ObjectId,
    pub b: ObjectId,
    pub frame: JointFrame,
    /// What the cut calls it ("+X face", "cut").
    pub label: String,
    pub transparent: JointSide,
    pub placements: Vec<Placement>,
    pub applied: Option<(RevisionId, RevisionId)>,
}

impl Joint {
    pub fn piece(&self, side: JointSide) -> ObjectId {
        match side {
            JointSide::A => self.a,
            JointSide::B => self.b,
        }
    }

    /// The solid piece (the one connectors are placed on).
    pub fn solid(&self) -> JointSide {
        self.transparent.other()
    }
}

/// A joint's pieces as the checks need them: meshes, placements, BVHs
/// and both cut faces, read on a background thread.
pub struct JointGeom {
    pub meshes: [Mesh; 2],
    pub transforms: [[f64; 12]; 2],
    pub bvhs: [Arc<Bvh>; 2],
    pub caps: [mf_operations::CapRegion; 2],
}

impl JointGeom {
    /// `side`'s own cap region (`A` first, `B` second, as every `[_; 2]`
    /// on this struct is ordered).
    pub fn cap(&self, side: JointSide) -> &mf_operations::CapRegion {
        match side {
            JointSide::A => &self.caps[0],
            JointSide::B => &self.caps[1],
        }
    }
}

#[derive(Clone)]
pub(crate) struct GeomKey {
    pub(crate) joint: usize,
    frame: JointFrame,
    meshes: [Mesh; 2],
    transforms: [[f64; 12]; 2],
}

impl GeomKey {
    fn same(&self, o: &GeomKey) -> bool {
        self.joint == o.joint
            && self.frame == o.frame
            && Mesh::ptr_eq(&self.meshes[0], &o.meshes[0])
            && Mesh::ptr_eq(&self.meshes[1], &o.meshes[1])
            && self.transforms == o.transforms
    }
}

type GeomResult = Arc<Mutex<Option<Result<JointGeom, String>>>>;

#[derive(Default)]
pub(crate) enum GeomSlot {
    #[default]
    Empty,
    Pending(GeomKey, GeomResult),
    Ready(GeomKey, Arc<JointGeom>),
    Failed(GeomKey, String),
}

/// A connector drag under way (Connect's own input).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Drag {
    /// Moving connector `index`, grabbed `grab` from its centre (2D).
    Move { index: usize, grab: [f64; 2] },
    /// Its rim: the diameter (width) follows the pointer.
    Size { index: usize },
    /// Its depth handle: up deepens, down makes it shallower.
    Depth {
        index: usize,
        from_y: f64,
        depth: f64,
    },
    /// Placing a dovetail by dragging a line across the cut face (owner
    /// 2026-11-11, D23): `start` is where the press landed; the current
    /// pointer (tracked by the caller, not stored here) is the line's
    /// other end until release, when `connect_tool::add_dovetail` turns
    /// the whole line into a placement.
    PlaceDovetail { start: [f64; 2] },
}

/// What the pointer is over in Connect.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Hit {
    Size(usize),
    Depth(usize),
    Connector(usize),
    Cap([f64; 2]),
    /// Another joint's cut face: a click opens it.
    Joint(usize),
}

/// One part of a connector as drawn: which side it's on (`None`: the
/// loose dowel) and its feature edges in the world.
#[derive(Debug, Clone)]
pub struct PreviewPart {
    pub side: Option<JointSide>,
    pub edges: Vec<[DVec3; 2]>,
}

/// The previews drawn: for which placements of which joint, and each
/// placement's parts.
pub(crate) type Previews = (Vec<Placement>, usize, Arc<Vec<Vec<PreviewPart>>>);

pub(crate) type ChecksKey = (Vec<Placement>, usize, [u64; 3], String, String);

/// The whole workflow's shared state (on the app as `split_flow`): the
/// cut's own settings (Split's) and the joints a cut makes, each joint's
/// connectors and live checks (Connect's).
#[derive(Default)]
pub struct Flow {
    pub mode: CutMode,
    pub volume: VolumeBox,
    pub joints: Vec<Joint>,
    /// The joint open in Connect.
    pub joint: Option<usize>,
    /// The connector kind a click places (a `joint.*` id).
    pub kind: Option<&'static str>,
    /// The connector picked (index into the open joint's placements).
    pub selected: Option<usize>,
    /// Which placed connectors' warnings are collapsed (shown by
    /// default; clicking a "N warnings" row toggles it shut, click again
    /// to reopen — owner 2026-11-11). Indices into the open joint's
    /// placements; stale ones (after a delete) are simply never hit.
    pub collapsed_warnings: std::collections::HashSet<usize>,
    /// The thinnest wall the checks want (mm).
    pub min_wall: Option<f64>,
    /// Connect's own Fit class (owner 2026-11-11: defaults to Sliding,
    /// a positive clearance, not Snug at 0 mm — independent of the
    /// Parts/Connectors tool's `app.connectors.fit_class`, so changing
    /// one doesn't silently change the other).
    pub fit_class: Option<String>,
    pub(crate) drag: Option<Drag>,
    pub(crate) hover: Option<Hit>,
    pub(crate) geom: GeomSlot,
    pub(crate) checks: Option<(ChecksKey, Arc<Vec<Vec<Check>>>)>,
    pub(crate) previews: Option<Previews>,
    /// The joint whose `split.place_connectors` is running.
    pub(crate) pending_apply: Option<usize>,
    /// The joint and solid side the camera was last turned to.
    pub(crate) framed: Option<(usize, JointSide)>,
    /// The piece count of the most recent cut, for the status line and
    /// the "Add connectors" link (cleared when a new cut is defined).
    pub(crate) last_cut: Option<(String, usize)>,
    /// The first joint the most recent cut made (the "Add connectors"
    /// link's target).
    pub(crate) first_new_joint: Option<usize>,
}

impl std::fmt::Debug for Flow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Flow")
            .field("mode", &self.mode)
            .field("joints", &self.joints)
            .field("joint", &self.joint)
            .field("selected", &self.selected)
            .finish_non_exhaustive()
    }
}

impl Flow {
    pub fn kind(&self) -> &'static str {
        self.kind
            .unwrap_or(mf_parts::connectors::joint::JOINT_CONNECTORS[0])
    }

    pub fn min_wall(&self) -> f64 {
        self.min_wall.unwrap_or(1.5)
    }

    /// Connect's Fit class name (Sliding by default: owner 2026-11-11,
    /// "not Snug at 0 mm").
    pub fn fit_class(&self) -> String {
        self.fit_class
            .clone()
            .unwrap_or_else(|| mf_parts::FitClass::builtins()[2].name.clone())
    }

    /// The open joint, if it still exists.
    pub fn open_joint(&self) -> Option<&Joint> {
        self.joint.and_then(|j| self.joints.get(j))
    }

    /// The open joint's pieces, once read.
    pub fn geometry(&self) -> Option<Arc<JointGeom>> {
        match &self.geom {
            GeomSlot::Ready(k, g) if Some(k.joint) == self.joint => Some(g.clone()),
            _ => None,
        }
    }

    /// The open joint's checks, per placement (empty until computed).
    pub fn checks(&self) -> Option<Arc<Vec<Vec<Check>>>> {
        self.checks.as_ref().map(|(_, c)| c.clone())
    }
}

/// A connector kind's name in the panel.
pub fn kind_label(id: &str) -> String {
    match id {
        "joint.dowel" => t!("connect.kind.dowel"),
        "joint.peg" => t!("connect.kind.peg"),
        "joint.dovetail" => t!("connect.kind.dovetail"),
        "joint.magnet" => t!("connect.kind.magnet"),
        _ => t!("connect.kind.insert"),
    }
}

/// A connector kind's plural noun, lower case ("dowels", "pegs"): for a
/// history row naming how many of one kind a step added (I5).
pub fn kind_label_plural(id: &str) -> String {
    match id {
        "joint.dowel" => t!("connect.kind.dowel.plural"),
        "joint.peg" => t!("connect.kind.peg.plural"),
        "joint.dovetail" => t!("connect.kind.dovetail.plural"),
        "joint.magnet" => t!("connect.kind.magnet.plural"),
        _ => t!("connect.kind.insert.plural"),
    }
}

/// Document units per millimetre.
pub fn mm(app: &MeshFixerApp) -> f64 {
    1.0 / crate::transform_tool::mm_per(app.session.document().units())
}

pub(crate) fn name_of(app: &MeshFixerApp, id: ObjectId) -> String {
    app.session
        .document()
        .object(id)
        .map_or_else(|| t!("split.the_model"), |o| o.name.clone())
}

pub(crate) fn exists(app: &MeshFixerApp, j: &Joint) -> bool {
    let doc = app.session.document();
    doc.object(j.a).is_some() && doc.object(j.b).is_some()
}

/// True if `id` is the open joint's transparent piece (drawn
/// see-through while Connect has it open).
pub fn is_ghost(app: &MeshFixerApp, id: ObjectId) -> bool {
    app.active_tool == crate::connect_tool::CONNECT
        && app
            .split_flow
            .open_joint()
            .is_some_and(|j| exists(app, j) && j.piece(j.transparent) == id)
}

/// Whether the joint's connectors are applied in the document as it is
/// now (the apply command is on the path to the current revision).
pub(crate) fn applied_now(app: &MeshFixerApp, j: &Joint) -> bool {
    j.applied.is_some_and(|(_, out)| {
        app.session
            .records_to_current()
            .iter()
            .any(|r| r.output_revision == out)
    })
}

// ---------------------------------------------------------------------
// After a command: joints from a cut, an applied joint.
// ---------------------------------------------------------------------

/// Record what a committed Split or Connect command means for the shared
/// state: a cut's joints (the status line's piece count, and the "Add
/// connectors" link's target), or a joint's connectors applied.
pub fn after_commit(app: &mut MeshFixerApp, op: &str, report: &serde_json::Value) {
    let records = app.session.records_to_current();
    let Some(record) = records.last() else { return };
    match op {
        "mesh.split_plane" | "mesh.split_box" => {
            let created: Vec<ObjectId> = record
                .outputs
                .iter()
                .filter(|o| o.created)
                .map(|o| o.object)
                .collect();
            let operands: Vec<ObjectId> = record.operands.iter().map(|o| o.object).collect();
            let piece = |r: &serde_json::Value| -> Option<ObjectId> {
                if let Some(k) = r["operand"].as_u64() {
                    operands.get(k as usize).copied()
                } else {
                    created.get(r["created"].as_u64()? as usize).copied()
                }
            };
            let vec3 = |v: &serde_json::Value| -> Option<DVec3> {
                let a = v.as_array()?;
                Some(DVec3::new(
                    a.first()?.as_f64()?,
                    a.get(1)?.as_f64()?,
                    a.get(2)?.as_f64()?,
                ))
            };
            let first = app.split_flow.joints.len();
            for j in report["joints"].as_array().into_iter().flatten() {
                let (Some(a), Some(b), Some(point), Some(normal)) = (
                    piece(&j["a"]),
                    piece(&j["b"]),
                    vec3(&j["point"]),
                    vec3(&j["normal"]),
                ) else {
                    continue;
                };
                let Some(gap) = j["gap"].as_f64() else {
                    continue;
                };
                let Some(frame) =
                    JointFrame::new(point, normal, JointFrame::default_u(normal), gap)
                else {
                    continue;
                };
                let label = j["face"].as_str().map_or_else(
                    || t!("connect.joint.cut"),
                    |f| t!("connect.joint.face", face = f),
                );
                app.split_flow.joints.push(Joint {
                    a,
                    b,
                    frame,
                    label,
                    transparent: JointSide::B,
                    placements: Vec::new(),
                    applied: None,
                });
            }
            // The box stays where the cut was made (not re-placed on the
            // piece the cut made active).
            if op == "mesh.split_box" {
                app.split_flow.volume.placed_for = Some(app.selected_objects());
            }
            let made = app.split_flow.joints.len() - first;
            if made > 0 {
                let name = app
                    .selected_objects()
                    .first()
                    .map_or_else(|| t!("split.the_model"), |&id| name_of(app, id));
                app.split_flow.last_cut = Some((name, made));
                app.split_flow.first_new_joint = Some(first);
            }
        }
        "split.place_connectors" => {
            if let Some(j) = app.split_flow.pending_apply.take()
                && let Some(joint) = app.split_flow.joints.get_mut(j)
            {
                joint.applied = Some((record.input_revision, record.output_revision));
            }
        }
        _ => {}
    }
}

/// Open joint `j` (in Connect: its geometry is read in the background,
/// and the camera turns to its solid cut face once it's in).
pub fn open_joint(app: &mut MeshFixerApp, j: usize) {
    let f = &mut app.split_flow;
    if f.joint != Some(j) {
        f.selected = None;
        f.drag = None;
        f.framed = None;
        f.collapsed_warnings.clear();
    }
    f.joint = Some(j);
}

/// Indices after removing placement `removed`: itself gone, everything
/// after it shifted down one (so `expanded_warnings` keeps naming the
/// same connectors once one is deleted).
fn reindex_after_removal(indices: &std::collections::HashSet<usize>, removed: usize) -> std::collections::HashSet<usize> {
    indices
        .iter()
        .filter(|&&i| i != removed)
        .map(|&i| if i > removed { i - 1 } else { i })
        .collect()
}

/// Remove connector `i` of the open joint (a row's trash button, or the
/// details block's "Remove connector").
pub fn remove_connector(app: &mut MeshFixerApp, j: usize, i: usize) {
    let f = &mut app.split_flow;
    let Some(joint) = f.joints.get_mut(j) else {
        return;
    };
    if i >= joint.placements.len() {
        return;
    }
    joint.placements.remove(i);
    f.collapsed_warnings = reindex_after_removal(&f.collapsed_warnings, i);
    if f.selected == Some(i) {
        f.selected = None;
    } else if let Some(s) = f.selected
        && s > i
    {
        f.selected = Some(s - 1);
    }
    f.drag = None;
}

/// Delete the picked connector (the Delete key, while Connect is open).
/// True if it did.
pub fn delete_selected(app: &mut MeshFixerApp) -> bool {
    if app.active_tool != crate::connect_tool::CONNECT {
        return false;
    }
    let f = &app.split_flow;
    let (Some(j), Some(i)) = (f.joint, f.selected) else {
        return false;
    };
    if i >= f.joints.get(j).map_or(0, |jt| jt.placements.len()) {
        return false;
    }
    remove_connector(app, j, i);
    true
}

// ---------------------------------------------------------------------
// Joint geometry (background) and the live checks: shared with Connect.
// ---------------------------------------------------------------------

/// Keep the open joint's geometry current: the pieces as they were before
/// its connectors were applied (if they're applied now), else as they
/// are; read on a background thread, reusing the app's picking BVHs.
pub(crate) fn refresh_geometry(app: &mut MeshFixerApp) {
    let Some(j) = app.split_flow.joint else {
        app.split_flow.geom = GeomSlot::Empty;
        return;
    };
    let Some(joint) = app.split_flow.joints.get(j).cloned() else {
        return;
    };
    let doc = match joint.applied.filter(|_| applied_now(app, &joint)) {
        Some((before, _)) => match app.session.node(before) {
            Some(n) => n.document.clone(),
            None => return,
        },
        None => app.session.document().clone(),
    };
    let (Some(a), Some(b)) = (doc.object(joint.a), doc.object(joint.b)) else {
        return;
    };
    let (Some(am), Some(bm)) = (a.mesh(), b.mesh()) else {
        return;
    };
    let key = GeomKey {
        joint: j,
        frame: joint.frame,
        meshes: [am.clone(), bm.clone()],
        transforms: [a.transform, b.transform],
    };
    let f = &mut app.split_flow;
    // Collect a finished read.
    let done = match &f.geom {
        GeomSlot::Pending(k, slot) => slot
            .lock()
            .ok()
            .and_then(|mut s| s.take())
            .map(|r| (k.clone(), r)),
        _ => None,
    };
    if let Some((k, result)) = done {
        f.geom = match result {
            Ok(g) => GeomSlot::Ready(k, Arc::new(g)),
            Err(e) => GeomSlot::Failed(k, e),
        };
        f.checks = None;
        f.previews = None;
    }
    let current = match &f.geom {
        GeomSlot::Pending(k, _) | GeomSlot::Ready(k, _) | GeomSlot::Failed(k, _) => {
            Some(k.same(&key))
        }
        GeomSlot::Empty => None,
    };
    if current == Some(true) {
        return;
    }
    let bvhs = [app.object_bvh(joint.a, am), app.object_bvh(joint.b, bm)];
    let slot: GeomResult = Arc::new(Mutex::new(None));
    let out = slot.clone();
    let k = key.clone();
    let repaint = app.repaint_context();
    std::thread::spawn(move || {
        let ctx = mf_runtime::RunContext::none();
        let mut built = Vec::new();
        for (i, b) in bvhs.into_iter().enumerate() {
            built.push(match b {
                Some(b) => Ok(b),
                None => Bvh::build(&k.meshes[i], &ctx).map(Arc::new),
            });
        }
        let result = (|| -> Result<JointGeom, String> {
            let ba = built.remove(0).map_err(|_| "cancelled".to_string())?;
            let bb = built.remove(0).map_err(|_| "cancelled".to_string())?;
            let caps = [
                mf_operations::cap_region(
                    &k.meshes[0],
                    Some(&ba),
                    &k.transforms[0],
                    &k.frame,
                    JointSide::A,
                    &ctx,
                )
                .map_err(|_| "cancelled".to_string())?,
                mf_operations::cap_region(
                    &k.meshes[1],
                    Some(&bb),
                    &k.transforms[1],
                    &k.frame,
                    JointSide::B,
                    &ctx,
                )
                .map_err(|_| "cancelled".to_string())?,
            ];
            Ok(JointGeom {
                meshes: k.meshes.clone(),
                transforms: k.transforms,
                bvhs: [ba, bb],
                caps,
            })
        })();
        if let Ok(mut s) = out.lock() {
            *s = Some(result);
        }
        if let Some(c) = repaint {
            c.request_repaint();
        }
    });
    app.split_flow.geom = GeomSlot::Pending(key, slot);
}

/// The checks' settings: Connect's own Fit class (Sliding by default)
/// and the Machine profile picked (the Connectors tool's own choice,
/// shared), the minimum wall, the units.
pub(crate) fn settings(app: &MeshFixerApp) -> mf_operations::CheckSettings {
    let (classes, _) = crate::connector_tool::fit_classes(app);
    let (profiles, _) = crate::connector_tool::machine_profiles(app);
    mf_operations::CheckSettings {
        min_wall: app.split_flow.min_wall(),
        fit_class: classes
            .get(&app.split_flow.fit_class())
            .cloned()
            .unwrap_or_else(|| mf_parts::FitClass::builtins()[2].clone()),
        machine: profiles
            .get(&app.connectors.machine_profile)
            .cloned()
            .unwrap_or_else(mf_parts::MachineProfile::uncalibrated),
        mm: mm(app),
    }
}

pub(crate) fn pieces<'a>(
    names: &'a [String; 2],
    g: &'a JointGeom,
) -> [mf_operations::Piece<'a>; 2] {
    [0, 1].map(|k| mf_operations::Piece {
        name: &names[k],
        mesh: &g.meshes[k],
        transform: g.transforms[k],
        bvh: &g.bvhs[k],
        cap: &g.caps[k],
    })
}

/// Keep the open joint's checks current (recomputed only when a
/// placement, the geometry or a setting changes).
pub(crate) fn refresh_checks(app: &mut MeshFixerApp) {
    let (Some(joint), Some(g)) = (
        app.split_flow.open_joint().cloned(),
        app.split_flow.geometry(),
    ) else {
        return;
    };
    let s = settings(app);
    let key: ChecksKey = (
        joint.placements.clone(),
        Arc::as_ptr(&g) as usize,
        [s.min_wall.to_bits(), s.mm.to_bits(), 0],
        s.fit_class.name.clone(),
        s.machine.name.clone(),
    );
    if app
        .split_flow
        .checks
        .as_ref()
        .is_some_and(|(k, _)| *k == key)
    {
        return;
    }
    let ctx = mf_runtime::RunContext::none();
    let names = [name_of(app, joint.a), name_of(app, joint.b)];
    let checks: Vec<Vec<Check>> = {
        let p = pieces(&names, &g);
        joint
            .placements
            .iter()
            .map(|pl| {
                mf_operations::check_placement(&joint.frame, [&p[0], &p[1]], pl, &s, &ctx)
                    .unwrap_or_else(|e| {
                        vec![Check {
                            kind: mf_operations::CheckKind::OffCap,
                            message: e.to_string(),
                        }]
                    })
            })
            .collect()
    };
    app.split_flow.checks = Some((key, Arc::new(checks)));
}

/// `w` flattened onto `solid`'s cap plane: its 2D place in the joint
/// frame, re-embedded at the solid cap's depth (owner 2026-11-11, I4).
/// A connector's own parts are generated on the solid cap already;
/// its counterpart is physically behind the *other* cap, a different
/// depth along the joint normal. Drawn at its true depth, a perspective
/// camera not centred on that connector shows it offset from its own
/// footprint by parallax; flattened, every part of one placement lands
/// at the same screen place its footprint does, and all of them are
/// exactly coincident when the view looks straight along the normal.
pub fn flatten_on_cap(frame: &JointFrame, solid: JointSide, w: DVec3) -> DVec3 {
    let lift = frame.cap_point(solid) - frame.point;
    frame.at(frame.to_2d(w)) + lift
}

/// The feature edges of a small closed mesh (its outline: edges between
/// faces turned more than about 25° apart).
pub(crate) fn feature_edges(mesh: &Mesh) -> Vec<[DVec3; 2]> {
    let mut by_edge: HashMap<(u32, u32), Vec<DVec3>> = HashMap::new();
    for (_, c) in mesh.faces() {
        let p = c.map(|v| DVec3::from(mesh.corner_position(v)));
        let n = (p[1] - p[0]).cross(p[2] - p[0]).normalize_or_zero();
        for i in 0..3 {
            let (a, b) = (c[i], c[(i + 1) % 3]);
            by_edge.entry((a.min(b), a.max(b))).or_default().push(n);
        }
    }
    let mut out: Vec<_> = by_edge
        .into_iter()
        .filter(|(_, n)| n.len() != 2 || n[0].dot(n[1]) < 0.9)
        .map(|((a, b), _)| {
            [
                DVec3::from(mesh.corner_position(a)),
                DVec3::from(mesh.corner_position(b)),
            ]
        })
        .collect();
    out.sort_by(|x, y| {
        x[0].to_array()
            .partial_cmp(&y[0].to_array())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

pub(crate) fn refresh_previews(app: &mut MeshFixerApp) {
    let Some(joint) = app.split_flow.open_joint().cloned() else {
        return;
    };
    let joint_index = app.split_flow.joint.unwrap_or(0);
    if app
        .split_flow
        .previews
        .as_ref()
        .is_some_and(|(p, j, _)| *p == joint.placements && *j == joint_index)
    {
        return;
    }
    let scale = mm(app);
    let previews: Vec<Vec<PreviewPart>> = joint
        .placements
        .iter()
        .map(|p| {
            mf_operations::placed_parts(&joint.frame, p, scale)
                .map(|parts| {
                    parts
                        .into_iter()
                        .map(|part| PreviewPart {
                            side: part.side,
                            edges: feature_edges(&part.world),
                        })
                        .collect()
                })
                .unwrap_or_default() // allow-swallow: an invalid placement's own check says why (check_placement runs the same generation); it just draws no preview
        })
        .collect();
    app.split_flow.previews = Some((joint.placements, joint_index, Arc::new(previews)));
}

/// Turn the camera to look straight at the open joint's solid cut face
/// (once per joint and side).
pub(crate) fn frame_joint(app: &mut MeshFixerApp) {
    let (Some(j), Some(joint), Some(g)) = (
        app.split_flow.joint,
        app.split_flow.open_joint().cloned(),
        app.split_flow.geometry(),
    ) else {
        return;
    };
    let solid = joint.solid();
    if app.split_flow.framed == Some((j, solid)) {
        return;
    }
    let Some(cam) = app.camera else { return };
    let cap = &g.caps[match solid {
        JointSide::A => 0,
        JointSide::B => 1,
    }];
    let Some((lo, hi)) = cap.bounds() else { return };
    let mut bb_lo = DVec3::splat(f64::INFINITY);
    let mut bb_hi = DVec3::splat(f64::NEG_INFINITY);
    for c in [
        [lo[0], lo[1]],
        [hi[0], lo[1]],
        [lo[0], hi[1]],
        [hi[0], hi[1]],
    ] {
        let w = joint.frame.at(c);
        bb_lo = bb_lo.min(w);
        bb_hi = bb_hi.max(w);
    }
    let target = Aabb {
        min: bb_lo.to_array(),
        max: bb_hi.to_array(),
    };
    let scene = Aabb {
        min: (bb_lo - (bb_hi - bb_lo) * 4.0 - DVec3::splat(1.0)).to_array(),
        max: (bb_hi + (bb_hi - bb_lo) * 4.0 + DVec3::splat(1.0)).to_array(),
    };
    // Facing the cut face, tipped about 25° towards the viewer's usual
    // side so the counterparts' depth into the see-through piece reads
    // (straight on, every hole is just a circle).
    let out = joint.frame.outward(solid);
    let side = (DVec3::new(0.3, -1.0, 0.4) - out * out.dot(DVec3::new(0.3, -1.0, 0.4)))
        .normalize_or(joint.frame.u);
    let from = (out + side * 0.45).normalize_or(out);
    app.camera = Some(cam.framing(&target, from, 0.75, &scene));
    app.split_flow.framed = Some((j, solid));
}

/// Everything Connect keeps up to date each frame, while it's open.
pub fn refresh(app: &mut MeshFixerApp) {
    if app.active_tool != crate::connect_tool::CONNECT {
        return;
    }
    if let Some(j) = app.split_flow.open_joint().cloned()
        && !exists(app, &j)
    {
        app.split_flow.joint = None;
    }
    refresh_geometry(app);
    if matches!(app.split_flow.geom, GeomSlot::Pending(..))
        && let Some(c) = app.repaint_context()
    {
        c.request_repaint_after(std::time::Duration::from_millis(30));
    }
    refresh_checks(app);
    refresh_previews(app);
    frame_joint(app);
}

// ---------------------------------------------------------------------
// The Split panel: cutting only.
// ---------------------------------------------------------------------

fn section(ui: &mut egui::Ui, text: &str) {
    ui.add_space(theme::SPACE_M);
    ui.label(
        RichText::new(text)
            .size(theme::TYPE_SECTION)
            .color(theme::TEXT_STRONG),
    );
    ui.add_space(2.0);
}

fn warning_card(ui: &mut egui::Ui, message: &str) {
    widgets::card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(icons::WARNING)
                    .font(icons::font(theme::ICON_SIZE))
                    .color(theme::LOOSE_PIECE),
            );
            ui.add(egui::Label::new(RichText::new(message).color(theme::TEXT)).wrap());
        });
    });
    ui.add_space(theme::SPACE_XS);
}

/// The Split panel: Mode (Slice | Print volume), the mode's own controls,
/// the pieces preview and miss/grazing warnings, then one Cut button.
/// After a cut, the status line reports the piece count and a secondary
/// link opens Connect on the new joints.
pub fn panel(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    if !crate::shape_tools::open_model_card(app, ui) {
        crate::shape_tools::refusal_card_for(app, ui, "mesh.split_plane");
    }
    section(ui, &t!("split.section.mode"));
    let modes = [t!("split.mode.slice"), t!("split.mode.volume")];
    let options: Vec<(&str, &str, bool)> = modes
        .iter()
        .map(|l| (l.as_str(), l.as_str(), true))
        .collect();
    let current = usize::from(app.split_flow.mode == CutMode::Volume);
    if let (Some(i), _) = widgets::segmented(ui, &options, current) {
        app.split_flow.mode = if i == 0 {
            CutMode::Slice
        } else {
            CutMode::Volume
        };
        app.split.drag = None;
        app.split_flow.volume.drag = None;
    }
    match app.split_flow.mode {
        CutMode::Slice => crate::shape_tools::slice_controls(app, ui),
        CutMode::Volume => volume_controls(app, ui),
    }
    cut_section(app, ui);
}

/// What the cut will make: the targets and result (Slice), or the piece
/// count along the box's faces (Print volume); then the Cut button, and
/// (once a cut's been made) the status line's piece count and the link
/// into Connect.
fn cut_section(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    let open = app.current_analysis().map(|a| a.diagnostics.is_closed) == Some(false);
    let n = app.selected_objects().len();
    section(ui, &t!("split.section.cut"));
    if app.split_flow.mode == CutMode::Volume {
        let pieces = box_pieces(app).unwrap_or_default(); // allow-swallow: no selection bounds yet means no list to show; Cut itself is refused by name
        widgets::body(ui, crate::tp!("split.cut.volume_summary", pieces.len()));
    } else {
        widgets::body(ui, crate::tp!("split.targets", n));
        widgets::caption(ui, t!("split.result"));
    }
    widgets::caption(ui, t!("split.cut.joints_help"));
    ui.add_space(theme::SPACE_L);
    let can = app.busy.is_none() && !open;
    let label = t!("split.apply");
    if ui
        .add_enabled_ui(can, |ui| {
            widgets::wide_button(ui, Kind::Primary, Some(icons::SPLIT), &label)
        })
        .inner
        .clicked()
    {
        apply_cut(app, label);
    }
    if let Some((name, n)) = app.split_flow.last_cut.clone() {
        ui.add_space(theme::SPACE_XS);
        widgets::caption(ui, crate::tp!("split.status.cut", n, name = name));
        if widgets::wide_button(
            ui,
            Kind::Secondary,
            Some(icons::CONNECT),
            &t!("split.connect_link"),
        )
        .clicked()
        {
            app.open_rail_tool(crate::connect_tool::CONNECT);
            let first = app
                .split_flow
                .first_new_joint
                .filter(|&j| app.split_flow.joints.get(j).is_some_and(|j| exists(app, j)))
                .or_else(|| app.split_flow.joints.iter().rposition(|j| exists(app, j)));
            if let Some(j) = first {
                open_joint(app, j);
            }
        }
    }
}

// --- Print volume ------------------------------------------------------

/// The printer presets the box offers: the Machine profiles' own volumes
/// first, then the built-in list.
fn presets(app: &MeshFixerApp) -> Vec<(String, [f64; 3])> {
    let (profiles, _) = crate::connector_tool::machine_profiles(app);
    profiles
        .profiles
        .iter()
        .map(|p| (p.name.clone(), p.print_volume_mm))
        .chain(
            mf_parts::PRINT_VOLUME_PRESETS
                .iter()
                .map(|(n, v)| ((*n).to_owned(), *v)),
        )
        .collect()
}

/// The world bounds of every selected object.
fn selection_bounds(app: &MeshFixerApp) -> Option<(DVec3, DVec3)> {
    let mut out: Option<(DVec3, DVec3)> = None;
    for id in app.selected_objects() {
        let Some((lo, hi)) = crate::shape_tools::world_bounds(app, id) else {
            continue;
        };
        out = Some(match out {
            Some((a, b)) => (a.min(lo), b.max(hi)),
            None => (lo, hi),
        });
    }
    out
}

/// Place the box: the chosen preset's size, centred on the selection,
/// resting on the plate (when the selection changed, or `force`).
pub fn place_box(app: &mut MeshFixerApp, force: bool) {
    let selected = app.selected_objects();
    if selected.is_empty() {
        return;
    }
    if !force && app.split_flow.volume.placed_for.as_deref() == Some(selected.as_slice()) {
        return;
    }
    let Some((lo, hi)) = selection_bounds(app) else {
        return;
    };
    let scale = mm(app);
    if app.split_flow.volume.preset.is_empty() {
        let all = presets(app);
        let chosen = all
            .iter()
            .find(|(n, _)| *n == app.connectors.machine_profile)
            .or(all.first())
            .cloned();
        if let Some((name, v)) = chosen {
            app.split_flow.volume.preset = name;
            app.split_flow.volume.size = DVec3::from(v) * scale;
        }
    }
    let v = &mut app.split_flow.volume;
    let c = (lo + hi) / 2.0;
    v.center = DVec3::new(c.x, c.y, v.size.z / 2.0);
    v.placed_for = Some(selected);
}

/// Where the box's faces would cut, piece by piece, from the selection's
/// bounds (the same face order as `mesh.split_box`): `(label, size)`,
/// the inside piece first. Bounds, so "about".
fn box_pieces(app: &MeshFixerApp) -> Option<Vec<(String, DVec3)>> {
    let (lo, hi) = selection_bounds(app)?;
    let v = &app.split_flow.volume;
    let (blo, bhi) = (v.min(), v.max());
    let (mut rlo, mut rhi) = (lo, hi);
    if (0..3).any(|k| rhi[k] <= blo[k] || rlo[k] >= bhi[k]) {
        return Some(Vec::new());
    }
    let mut out = Vec::new();
    for (axis, sign, label) in mf_operations::BOX_FACES {
        if sign < 0.0 && rlo[axis] < blo[axis] {
            let mut ohi = rhi;
            ohi[axis] = blo[axis];
            out.push((t!("split.piece.outside", face = label), ohi - rlo));
            rlo[axis] = blo[axis];
        } else if sign > 0.0 && rhi[axis] > bhi[axis] {
            let mut olo = rlo;
            olo[axis] = bhi[axis];
            out.push((t!("split.piece.outside", face = label), rhi - olo));
            rhi[axis] = bhi[axis];
        }
    }
    if out.is_empty() {
        return Some(vec![(t!("split.piece.inside"), rhi - rlo)]);
    }
    out.insert(0, (t!("split.piece.inside"), rhi - rlo));
    Some(out)
}

/// Why the box can't cut, shown before Cut is pressed: it misses the
/// model, or the model already fits.
fn box_warning(app: &MeshFixerApp) -> Option<String> {
    let pieces = box_pieces(app)?;
    let name = app
        .selected_objects()
        .first()
        .map_or_else(|| t!("split.the_model"), |&id| name_of(app, id));
    if pieces.is_empty() {
        Some(t!("split.box.misses", name = name))
    } else if pieces.len() == 1 {
        Some(t!("split.box.fits", name = name))
    } else {
        None
    }
}

fn volume_controls(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    place_box(app, false);
    let units = crate::defects::unit_symbol(app.session.document().units());
    let scale = mm(app);
    if let Some(w) = box_warning(app) {
        warning_card(ui, &w);
    }
    widgets::caption(ui, t!("split.box.help"));
    section(ui, &t!("split.section.printer"));
    let all = presets(app);
    let custom = t!("split.preset.custom");
    let shown = if app.split_flow.volume.preset.is_empty() {
        custom.clone()
    } else {
        app.split_flow.volume.preset.clone()
    };
    egui::ComboBox::from_id_salt("print volume preset")
        .selected_text(shown)
        .width(ui.available_width())
        .show_ui(ui, |ui| {
            for (name, v) in &all {
                let label = format!("{name} ({:.0}×{:.0}×{:.0} mm)", v[0], v[1], v[2]);
                if ui
                    .selectable_label(app.split_flow.volume.preset == *name, label)
                    .clicked()
                {
                    let f = &mut app.split_flow.volume;
                    f.preset = name.clone();
                    let z0 = f.min().z;
                    f.size = DVec3::from(*v) * scale;
                    f.center.z = z0 + f.size.z / 2.0;
                }
            }
            if ui
                .selectable_label(app.split_flow.volume.preset == custom, custom.clone())
                .clicked()
            {
                app.split_flow.volume.preset = custom.clone();
            }
        });
    section(ui, &t!("split.section.size", unit = units));
    let mut size = app.split_flow.volume.size;
    let mut changed = false;
    ui.horizontal(|ui| {
        for (k, name) in ["W", "D", "H"].into_iter().enumerate() {
            ui.label(
                RichText::new(name)
                    .size(theme::TYPE_CAPTION)
                    .color(theme::TEXT_MUTED),
            );
            let r = ui.add(
                egui::DragValue::new(&mut size[k])
                    .speed(1.0)
                    .range(1.0 * scale..=10_000.0 * scale)
                    .max_decimals(1),
            );
            let label = format!("Print volume {name}");
            r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::SpinButton, true, &label));
            changed |= r.changed();
        }
    });
    if changed {
        let f = &mut app.split_flow.volume;
        let z0 = f.min().z;
        f.size = size;
        f.center.z = z0 + size.z / 2.0;
        f.preset = custom.clone();
    }
    section(ui, &t!("split.section.position", unit = units));
    let mut c = app.split_flow.volume.center;
    let mut moved = false;
    ui.horizontal(|ui| {
        for k in 0..3 {
            let color = [theme::AXIS_X, theme::AXIS_Y, theme::AXIS_Z][k];
            ui.label(
                RichText::new(crate::transform_tool::AXIS_NAMES[k])
                    .size(theme::TYPE_CAPTION)
                    .color(color),
            );
            let r = ui.add(egui::DragValue::new(&mut c[k]).speed(0.5).max_decimals(2));
            let label = format!("Box centre {}", crate::transform_tool::AXIS_NAMES[k]);
            r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::SpinButton, true, &label));
            moved |= r.changed();
        }
    });
    if moved {
        app.split_flow.volume.center = c;
    }
    ui.horizontal(|ui| {
        let w = (ui.available_width() - ui.spacing().item_spacing.x) / 2.0;
        if widgets::button(ui, Kind::Secondary, None, &t!("split.box.centre"), w).clicked() {
            place_box(app, true);
        }
        if widgets::button(ui, Kind::Secondary, None, &t!("split.box.plate"), w).clicked() {
            let f = &mut app.split_flow.volume;
            f.center.z = f.size.z / 2.0;
        }
    });
    section(ui, &t!("split.section.pieces"));
    match box_pieces(app) {
        Some(pieces) if pieces.len() > 1 => {
            for (label, s) in pieces {
                let s = s / scale;
                widgets::body(
                    ui,
                    t!(
                        "split.piece.size",
                        piece = label,
                        w = format!("{:.0}", s.x),
                        d = format!("{:.0}", s.y),
                        h = format!("{:.0}", s.z),
                    ),
                );
            }
        }
        _ => {
            widgets::caption(ui, t!("split.pieces.none"));
        }
    }
}

fn vec3(v: DVec3) -> String {
    format!("{},{},{}", v.x, v.y, v.z)
}

/// Cut: `mesh.split_plane` or `mesh.split_box` on every selected object,
/// one command.
fn apply_cut(app: &mut MeshFixerApp, label: String) {
    let selected = app.selected_objects();
    let Some((&first, rest)) = selected.split_first() else {
        return;
    };
    let more = rest
        .iter()
        .map(|&id| (mf_command::TARGET_OPERAND.to_owned(), id, None))
        .collect();
    let (op, params) = match app.split_flow.mode {
        CutMode::Slice => {
            let s = &app.split;
            (
                "mesh.split_plane",
                BTreeMap::from([
                    ("plane_point".to_owned(), vec3(s.point())),
                    ("plane_normal".to_owned(), vec3(s.normal())),
                    ("keep".to_owned(), s.keep.clone()),
                    ("cut_gap".to_owned(), s.gap.to_string()),
                ]),
            )
        }
        CutMode::Volume => {
            let v = &app.split_flow.volume;
            (
                "mesh.split_box",
                BTreeMap::from([
                    ("box_min".to_owned(), vec3(v.min())),
                    ("box_max".to_owned(), vec3(v.max())),
                    ("cut_gap".to_owned(), app.split.gap.to_string()),
                ]),
            )
        }
    };
    app.refusal = None;
    app.split_flow.last_cut = None;
    app.run_invocation(
        Invocation {
            op_id: op.into(),
            target: first,
            selection: None,
            operands: more,
            raw_params: params,
        },
        label,
    );
}

// ---------------------------------------------------------------------
// The view: the box's gizmo and face handles (Connect draws its own
// connectors in `crate::connect_tool`).
// ---------------------------------------------------------------------

/// The box's gizmo (move only) in Split's Print-volume mode.
pub fn box_gizmo(app: &MeshFixerApp) -> Option<(Gizmo, Option<Handle>)> {
    let f = &app.split_flow;
    if app.active_tool != crate::shape_tools::SPLIT || f.mode != CutMode::Volume {
        return None;
    }
    let v = &f.volume;
    Some((
        Gizmo {
            pivot: v.center,
            mode: GizmoMode::Move,
            size_pt: crate::transform_tool::GIZMO_SIZE_PT,
        },
        v.drag.map(|(d, _)| d.handle()).or(v.hover),
    ))
}

pub(crate) fn view(app: &MeshFixerApp) -> Option<(mf_render::OrbitCamera, (f64, f64))> {
    let cam = app.camera?;
    let rect = app.viewport.last_rect?;
    Some((cam, (rect.width() as f64, rect.height() as f64)))
}

pub(crate) fn screen(cam: &mf_render::OrbitCamera, size: (f64, f64), p: DVec3) -> Option<(f64, f64)> {
    mf_render::screen_position(cam, cam.target, size, p.to_array())
}

/// The face handle under `px` (within 9 points of its centre).
fn face_hit(app: &MeshFixerApp, px: (f64, f64)) -> Option<usize> {
    let (cam, size) = view(app)?;
    let v = &app.split_flow.volume;
    (0..6)
        .filter_map(|k| {
            let (c, _) = v.face(k);
            let s = screen(&cam, size, c)?;
            let d = ((s.0 - px.0).powi(2) + (s.1 - px.1).powi(2)).sqrt();
            (d < 9.0).then_some((k, d))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(k, _)| k)
}

/// Where along the line `c + s·axis` the pointer's ray passes closest.
fn along_axis(app: &MeshFixerApp, c: DVec3, axis: DVec3, px: (f64, f64)) -> Option<f64> {
    let (cam, size) = view(app)?;
    let (o, d) = cam.ray(cam.target, size, px);
    let (o, d) = (DVec3::from(o), DVec3::from(d));
    let w0 = c - o;
    let (a, b, cc) = (axis.dot(axis), axis.dot(d), d.dot(d));
    let (dd, e) = (axis.dot(w0), d.dot(w0));
    let denom = a * cc - b * b;
    if denom.abs() < 1e-12 {
        return None;
    }
    Some((b * e - cc * dd) / denom)
}

/// The press is Split's own: a gizmo handle, or a box face handle.
pub fn claims(app: &MeshFixerApp) -> Option<Claims> {
    if app.busy.is_some() {
        return None;
    }
    let f = &app.split_flow;
    if f.mode == CutMode::Volume {
        let (g, _) = box_gizmo(app)?;
        let (cam, size) = view(app)?;
        let faces: Vec<(f64, f64)> = (0..6)
            .filter_map(|k| screen(&cam, size, f.volume.face(k).0))
            .collect();
        Some(Box::new(move |px: (f64, f64)| {
            g.hit(&cam, size, px).is_some()
                || faces
                    .iter()
                    .any(|s| ((s.0 - px.0).powi(2) + (s.1 - px.1).powi(2)).sqrt() < 9.0)
        }) as Claims)
    } else {
        let (g, _) = crate::shape_tools::split_gizmo(app)?;
        let (cam, size) = view(app)?;
        Some(Box::new(move |px: (f64, f64)| g.hit(&cam, size, px).is_some()) as Claims)
    }
}

/// Split's input in the view.
pub fn input(app: &mut MeshFixerApp, input: &crate::shell::ToolInput) -> Input {
    if app.split_flow.mode == CutMode::Volume {
        volume_input(app, input)
    } else {
        crate::shape_tools::plane_input(app, input)
    }
}

fn volume_input(app: &mut MeshFixerApp, input: &crate::shell::ToolInput) -> Input {
    use crate::viewport::ToolPointer;
    let Some((g, _)) = box_gizmo(app) else {
        return Input::Pass;
    };
    let Some((cam, size)) = view(app) else {
        return Input::Pass;
    };
    let snap = if input.modifiers.command {
        Snap {
            grid: None,
            angle_deg: None,
            scale_step: None,
            plate: None,
        }
    } else {
        Snap {
            plate: None,
            ..Snap::default()
        }
    };
    let min_size = mm(app);
    match input.pointer {
        Some(ToolPointer::Start { from, at }) => {
            if let Some(k) = face_hit(app, from) {
                let v = &app.split_flow.volume;
                let (c, n) = v.face(k);
                if let Some(s) = along_axis(app, c, n, from) {
                    let (lo, hi) = (v.min(), v.max());
                    app.split_flow.volume.face_drag = Some((k, lo, hi, s));
                }
            } else if let Some(h) = g.hit(&cam, size, from) {
                let v = &mut app.split_flow.volume;
                let start = mf_render::compose(v.center, DVec3::ZERO, DVec3::ONE, DVec3::ZERO);
                let drag = g.begin(h, from, &cam, size, start);
                let t = drag.update(at, &snap);
                v.center = DVec3::new(t[9], t[10], t[11]);
                v.drag = Some((drag, start));
            }
        }
        Some(ToolPointer::Move(px)) | Some(ToolPointer::End(px)) => {
            if let Some((k, lo, hi, grab)) = app.split_flow.volume.face_drag {
                let (axis, sign, _) = mf_operations::BOX_FACES[k];
                let mut n = DVec3::ZERO;
                n[axis] = sign;
                let c = if sign > 0.0 { hi } else { lo };
                let mut face = (lo + hi) / 2.0;
                face[axis] = c[axis];
                if let Some(s) = along_axis(app, face, n, px) {
                    let (mut lo, mut hi) = (lo, hi);
                    if sign > 0.0 {
                        hi[axis] = (hi[axis] + (s - grab)).max(lo[axis] + min_size);
                    } else {
                        lo[axis] = (lo[axis] - (s - grab)).min(hi[axis] - min_size);
                    }
                    let v = &mut app.split_flow.volume;
                    v.center = (lo + hi) / 2.0;
                    v.size = hi - lo;
                    v.preset = t!("split.preset.custom");
                }
            } else if let Some((drag, _)) = app.split_flow.volume.drag {
                let t = drag.update(px, &snap);
                app.split_flow.volume.center = DVec3::new(t[9], t[10], t[11]);
            }
            if matches!(input.pointer, Some(ToolPointer::End(_))) {
                app.split_flow.volume.drag = None;
                app.split_flow.volume.face_drag = None;
            }
        }
        Some(ToolPointer::Click(_)) | None => {}
    }
    let busy = app.split_flow.volume.drag.is_some() || app.split_flow.volume.face_drag.is_some();
    let hover_face = if busy {
        None
    } else {
        input.hover.and_then(|px| face_hit(app, px))
    };
    let v = &mut app.split_flow.volume;
    v.hover_face = hover_face;
    v.hover = if busy || hover_face.is_some() {
        None
    } else {
        input.hover.and_then(|px| g.hit(&cam, size, px))
    };
    Input::Consumed
}

/// The box (Print volume), over the view, while Split is open.
pub fn overlay(app: &MeshFixerApp, painter: &egui::Painter, rect: egui::Rect) {
    let Some((cam, size)) = view(app) else { return };
    let at = |p: DVec3| {
        screen(&cam, size, p).map(|(x, y)| rect.left_top() + egui::vec2(x as f32, y as f32))
    };
    let f = &app.split_flow;
    if app.active_tool == crate::shape_tools::SPLIT && f.mode == CutMode::Volume {
        draw_box(app, painter, &cam, &at);
    }
}

fn draw_box(
    app: &MeshFixerApp,
    painter: &egui::Painter,
    cam: &mf_render::OrbitCamera,
    at: &dyn Fn(DVec3) -> Option<egui::Pos2>,
) {
    let v = &app.split_flow.volume;
    let (lo, hi) = (v.min(), v.max());
    let corner = |i: usize| {
        DVec3::new(
            if i & 1 == 0 { lo.x } else { hi.x },
            if i & 2 == 0 { lo.y } else { hi.y },
            if i & 4 == 0 { lo.z } else { hi.z },
        )
    };
    let warn = box_warning(app).is_some();
    let colour = if warn {
        theme::LOOSE_PIECE
    } else {
        theme::ACCENT
    };
    // Faces back to front, each about 12% so two layers read as ~25%.
    let faces: [[usize; 4]; 6] = [
        [0, 2, 6, 4],
        [1, 3, 7, 5],
        [0, 1, 5, 4],
        [2, 3, 7, 6],
        [0, 1, 3, 2],
        [4, 5, 7, 6],
    ];
    let eye = cam.eye();
    let mut order: Vec<(f64, [usize; 4])> = faces
        .iter()
        .map(|f| {
            let c = f.iter().map(|&i| corner(i)).sum::<DVec3>() / 4.0;
            ((c - eye).length(), *f)
        })
        .collect();
    order.sort_by(|a, b| b.0.total_cmp(&a.0));
    for (_, f) in order {
        let pts: Option<Vec<egui::Pos2>> = f.iter().map(|&i| at(corner(i))).collect();
        if let Some(pts) = pts {
            painter.add(egui::Shape::convex_polygon(
                pts,
                colour.gamma_multiply(0.12),
                egui::Stroke::NONE,
            ));
        }
    }
    for (a, b) in [
        (0, 1),
        (2, 3),
        (4, 5),
        (6, 7),
        (0, 2),
        (1, 3),
        (4, 6),
        (5, 7),
        (0, 4),
        (1, 5),
        (2, 6),
        (3, 7),
    ] {
        if let (Some(p), Some(q)) = (at(corner(a)), at(corner(b))) {
            painter.line_segment([p, q], egui::Stroke::new(1.5, colour.gamma_multiply(0.9)));
        }
    }
    // Face handles.
    for k in 0..6 {
        let (c, _) = v.face(k);
        let Some(p) = at(c) else { continue };
        let on = v.hover_face == Some(k) || v.face_drag.is_some_and(|d| d.0 == k);
        painter.circle(
            p,
            if on { 7.0 } else { 5.5 },
            if on { theme::HOVER } else { colour },
            egui::Stroke::new(1.5, theme::VIEWPORT_BACKGROUND),
        );
    }
}
