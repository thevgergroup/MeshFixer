//! The Split tool's three-step workflow (owner 2026-11-11): the panel's
//! step tabs "1 Define cut · 2 Cut · 3 Connectors", back and forward.
//!
//! 1. **Define the cut.** *Slice*: one plane, placed with the gizmo and
//!    the Horizontal/Vertical/tilt presets (`shape_tools`' plane
//!    controls). *Print volume*: a translucent box (about 25% opaque,
//!    edges drawn) moved with the gizmo and resized with a handle on each
//!    face, or typed W×D×H, from the printer presets (the Machine
//!    profiles' own volumes first, then `mf_parts::PRINT_VOLUME_PRESETS`).
//!    The box's faces are the cut planes; the panel lists the pieces it
//!    would make with their sizes. No automatic suggestions.
//! 2. **Cut.** `mesh.split_plane` or `mesh.split_box`: every cut capped
//!    flat, one undo step. Each cut face pair the command reports becomes
//!    a [`Joint`]; "Next: Connectors" goes on to the first new one.
//! 3. **Connectors, per joint.** Pick a joint (the list, or click another
//!    joint's cut face in the view), pick which piece is transparent (drawn see-through, the
//!    other piece's cut face solid and facing the view), and click on
//!    that face to place a connector of the chosen kind (the registry's
//!    `joint.*` connectors). Each can be picked, dragged, resized (a
//!    handle on its rim) and deepened (the square handle beside it), or
//!    deleted. Its counterpart on the transparent side is drawn live,
//!    from the very parts `split.place_connectors` will build. Live checks
//!    (breakout, edge distance, dowel length, fit) show per connector in
//!    plain words, each with a one-click fix. Apply runs
//!    `split.place_connectors` once; a joint can be reopened and applied
//!    again (from the revision before its connectors).

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, RichText};
use glam::DVec3;
use mf_command::{Invocation, ObjectId, RevisionId};
use mf_geometry::{Aabb, Bvh, Mesh};
use mf_operations::{
    CapRegion, Check, CheckKind, CheckSettings, JointFrame, JointSide, Piece, Placement,
};
use mf_render::{Gizmo, GizmoMode, Handle, Snap};

use crate::MeshFixerApp;
use crate::shell::{Claims, Input};
use crate::widgets::{self, Kind};
use crate::{icons, t, theme};

// ---------------------------------------------------------------------
// State.
// ---------------------------------------------------------------------

/// Which step of the workflow the panel shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Step {
    #[default]
    Define,
    Cut,
    Connectors,
}

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
/// connectors placed on it, and (once applied) the revisions before and
/// after `split.place_connectors`.
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
    pub caps: [CapRegion; 2],
}

#[derive(Clone)]
struct GeomKey {
    joint: usize,
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
enum GeomSlot {
    #[default]
    Empty,
    Pending(GeomKey, GeomResult),
    Ready(GeomKey, Arc<JointGeom>),
    Failed(GeomKey, String),
}

/// A connector drag under way.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Drag {
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
}

/// What the pointer is over in step 3.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Hit {
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
type Previews = (Vec<Placement>, usize, Arc<Vec<Vec<PreviewPart>>>);

type ChecksKey = (Vec<Placement>, usize, [u64; 3], String, String);

/// The whole workflow's state (on the app as `split_flow`).
#[derive(Default)]
pub struct Flow {
    pub step: Step,
    pub mode: CutMode,
    pub volume: VolumeBox,
    pub joints: Vec<Joint>,
    /// The joint open in step 3.
    pub joint: Option<usize>,
    /// The connector kind a click places (a `joint.*` id).
    pub kind: Option<&'static str>,
    /// The connector picked (index into the open joint's placements).
    pub selected: Option<usize>,
    /// The thinnest wall the checks want (mm).
    pub min_wall: Option<f64>,
    drag: Option<Drag>,
    hover: Option<Hit>,
    geom: GeomSlot,
    checks: Option<(ChecksKey, Arc<Vec<Vec<Check>>>)>,
    previews: Option<Previews>,
    /// The joint whose `split.place_connectors` is running.
    pending_apply: Option<usize>,
    /// The joint and solid side the camera was last turned to.
    framed: Option<(usize, JointSide)>,
}

impl std::fmt::Debug for Flow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Flow")
            .field("step", &self.step)
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
        "joint.dowel" => t!("splitw.kind.dowel"),
        "joint.peg" => t!("splitw.kind.peg"),
        "joint.dovetail" => t!("splitw.kind.dovetail"),
        "joint.magnet" => t!("splitw.kind.magnet"),
        _ => t!("splitw.kind.insert"),
    }
}

/// Document units per millimetre.
fn mm(app: &MeshFixerApp) -> f64 {
    1.0 / crate::transform_tool::mm_per(app.session.document().units())
}

fn name_of(app: &MeshFixerApp, id: ObjectId) -> String {
    app.session
        .document()
        .object(id)
        .map_or_else(|| t!("split.the_model"), |o| o.name.clone())
}

fn exists(app: &MeshFixerApp, j: &Joint) -> bool {
    let doc = app.session.document();
    doc.object(j.a).is_some() && doc.object(j.b).is_some()
}

/// True if `id` is the open joint's transparent piece (drawn
/// see-through in step 3).
pub fn is_ghost(app: &MeshFixerApp, id: ObjectId) -> bool {
    app.active_tool == crate::shape_tools::SPLIT
        && app.split_flow.step == Step::Connectors
        && app
            .split_flow
            .open_joint()
            .is_some_and(|j| exists(app, j) && j.piece(j.transparent) == id)
}

/// Whether the joint's connectors are applied in the document as it is
/// now (the apply command is on the path to the current revision).
fn applied_now(app: &MeshFixerApp, j: &Joint) -> bool {
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

/// Record what a committed Split command means for the workflow: a cut's
/// joints (and on to step 3 with the first one open), or a joint's
/// connectors applied.
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
                    || t!("splitw.joint.cut"),
                    |f| t!("splitw.joint.face", face = f),
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
            // The first new joint is the one step 3 opens (the user goes
            // on to it from the Cut step, or picks another there).
            if app.split_flow.joints.len() > first {
                open_joint(app, first);
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

/// Open joint `j` in step 3 (its geometry is read in the background, and
/// the camera turns to its solid cut face once it's in).
pub fn open_joint(app: &mut MeshFixerApp, j: usize) {
    let f = &mut app.split_flow;
    if f.joint != Some(j) {
        f.selected = None;
        f.drag = None;
        f.framed = None;
    }
    f.joint = Some(j);
}

/// Delete the picked connector (the Delete key in step 3). True if it
/// did.
pub fn delete_selected(app: &mut MeshFixerApp) -> bool {
    if app.active_tool != crate::shape_tools::SPLIT || app.split_flow.step != Step::Connectors {
        return false;
    }
    let f = &mut app.split_flow;
    let (Some(j), Some(i)) = (f.joint, f.selected) else {
        return false;
    };
    let Some(joint) = f.joints.get_mut(j) else {
        return false;
    };
    if i >= joint.placements.len() {
        return false;
    }
    joint.placements.remove(i);
    f.selected = None;
    f.drag = None;
    true
}

// ---------------------------------------------------------------------
// Joint geometry (background) and the live checks.
// ---------------------------------------------------------------------

/// Keep the open joint's geometry current: the pieces as they were before
/// its connectors were applied (if they're applied now), else as they
/// are; read on a background thread, reusing the app's picking BVHs.
fn refresh_geometry(app: &mut MeshFixerApp) {
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

/// The checks' settings: the Fit class and Machine profile picked (the
/// Connectors tool's pickers, shared), the minimum wall, the units.
fn settings(app: &MeshFixerApp) -> CheckSettings {
    let (classes, _) = crate::connector_tool::fit_classes(app);
    let (profiles, _) = crate::connector_tool::machine_profiles(app);
    CheckSettings {
        min_wall: app.split_flow.min_wall(),
        fit_class: classes
            .get(&app.connectors.fit_class)
            .cloned()
            .unwrap_or_else(|| mf_parts::FitClass::builtins()[1].clone()),
        machine: profiles
            .get(&app.connectors.machine_profile)
            .cloned()
            .unwrap_or_else(mf_parts::MachineProfile::uncalibrated),
        mm: mm(app),
    }
}

fn pieces<'a>(names: &'a [String; 2], g: &'a JointGeom) -> [Piece<'a>; 2] {
    [0, 1].map(|k| Piece {
        name: &names[k],
        mesh: &g.meshes[k],
        transform: g.transforms[k],
        bvh: &g.bvhs[k],
        cap: &g.caps[k],
    })
}

/// Keep the open joint's checks current (recomputed only when a
/// placement, the geometry or a setting changes).
fn refresh_checks(app: &mut MeshFixerApp) {
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
                            kind: CheckKind::OffCap,
                            message: e.to_string(),
                        }]
                    })
            })
            .collect()
    };
    app.split_flow.checks = Some((key, Arc::new(checks)));
}

/// The feature edges of a small closed mesh (its outline: edges between
/// faces turned more than about 25° apart).
fn feature_edges(mesh: &Mesh) -> Vec<[DVec3; 2]> {
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

fn refresh_previews(app: &mut MeshFixerApp) {
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
fn frame_joint(app: &mut MeshFixerApp) {
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

/// Everything step 3 keeps up to date each frame.
pub fn refresh(app: &mut MeshFixerApp) {
    if app.active_tool != crate::shape_tools::SPLIT || app.split_flow.step != Step::Connectors {
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
// The panel.
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

/// The Split panel: the step tabs, then the step's own controls.
pub fn panel(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    refresh(app);
    let labels = [
        t!("splitw.step.define"),
        t!("splitw.step.cut"),
        t!("splitw.step.connectors"),
    ];
    let options: Vec<(&str, &str, bool)> = labels
        .iter()
        .map(|l| (l.as_str(), l.as_str(), true))
        .collect();
    let current = match app.split_flow.step {
        Step::Define => 0,
        Step::Cut => 1,
        Step::Connectors => 2,
    };
    if let (Some(i), _) = widgets::segmented(ui, &options, current) {
        set_step(app, [Step::Define, Step::Cut, Step::Connectors][i]);
    }
    ui.add_space(theme::SPACE_XS);
    if !crate::shape_tools::open_model_card(app, ui) {
        crate::shape_tools::refusal_card_for(app, ui, "mesh.split_plane");
    }
    match app.split_flow.step {
        Step::Define => define_panel(app, ui),
        Step::Cut => cut_panel(app, ui),
        Step::Connectors => connectors_panel(app, ui),
    }
}

/// Go to step `step` (drags in progress end).
pub fn set_step(app: &mut MeshFixerApp, step: Step) {
    let f = &mut app.split_flow;
    f.step = step;
    f.drag = None;
    f.volume.drag = None;
    f.volume.face_drag = None;
    app.split.drag = None;
    if step == Step::Connectors {
        // Back in step 3: turn to the joint again.
        app.split_flow.framed = None;
        if app.split_flow.joint.is_none() {
            let first = app.split_flow.joints.iter().rposition(|j| exists(app, j));
            if let Some(j) = first {
                open_joint(app, j);
            }
        }
    }
}

fn define_panel(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    section(ui, &t!("splitw.section.mode"));
    let modes = [t!("splitw.mode.slice"), t!("splitw.mode.volume")];
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
    ui.add_space(theme::SPACE_L);
    if widgets::wide_button(ui, Kind::Secondary, None, &t!("splitw.next_cut")).clicked() {
        set_step(app, Step::Cut);
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
            out.push((t!("splitw.piece.outside", face = label), ohi - rlo));
            rlo[axis] = blo[axis];
        } else if sign > 0.0 && rhi[axis] > bhi[axis] {
            let mut olo = rlo;
            olo[axis] = bhi[axis];
            out.push((t!("splitw.piece.outside", face = label), rhi - olo));
            rhi[axis] = bhi[axis];
        }
    }
    if out.is_empty() {
        return Some(vec![(t!("splitw.piece.inside"), rhi - rlo)]);
    }
    out.insert(0, (t!("splitw.piece.inside"), rhi - rlo));
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
        Some(t!("splitw.box.misses", name = name))
    } else if pieces.len() == 1 {
        Some(t!("splitw.box.fits", name = name))
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
    widgets::caption(ui, t!("splitw.box.help"));
    section(ui, &t!("splitw.section.printer"));
    let all = presets(app);
    let custom = t!("splitw.preset.custom");
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
    section(ui, &t!("splitw.section.size", unit = units));
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
    section(ui, &t!("splitw.section.position", unit = units));
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
        if widgets::button(ui, Kind::Secondary, None, &t!("splitw.box.centre"), w).clicked() {
            place_box(app, true);
        }
        if widgets::button(ui, Kind::Secondary, None, &t!("splitw.box.plate"), w).clicked() {
            let f = &mut app.split_flow.volume;
            f.center.z = f.size.z / 2.0;
        }
    });
    section(ui, &t!("splitw.section.pieces"));
    match box_pieces(app) {
        Some(pieces) if pieces.len() > 1 => {
            for (label, s) in pieces {
                let s = s / scale;
                widgets::body(
                    ui,
                    t!(
                        "splitw.piece.size",
                        piece = label,
                        w = format!("{:.0}", s.x),
                        d = format!("{:.0}", s.y),
                        h = format!("{:.0}", s.z),
                    ),
                );
            }
        }
        _ => {
            widgets::caption(ui, t!("splitw.pieces.none"));
        }
    }
}

// --- Step 2: Cut --------------------------------------------------------

fn cut_panel(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    let open = app.current_analysis().map(|a| a.diagnostics.is_closed) == Some(false);
    let n = app.selected_objects().len();
    let volume = app.split_flow.mode == CutMode::Volume;
    section(ui, &t!("splitw.section.cut"));
    if volume {
        place_box(app, false);
        if let Some(w) = box_warning(app) {
            warning_card(ui, &w);
        }
        let pieces = box_pieces(app).unwrap_or_default(); // allow-swallow: no selection bounds yet means no list to show; Cut itself is refused by name
        widgets::body(ui, crate::tp!("splitw.cut.volume_summary", pieces.len()));
    } else {
        crate::shape_tools::place_plane(app, false);
        crate::shape_tools::plane_miss_warning(app, ui);
        widgets::body(ui, crate::tp!("split.targets", n));
        widgets::caption(ui, t!("split.result"));
    }
    widgets::caption(ui, t!("splitw.cut.joints_help"));
    ui.add_space(theme::SPACE_L);
    let can = app.busy.is_none() && !open;
    let label = if volume {
        t!("splitw.cut.apply_volume")
    } else {
        t!("split.apply")
    };
    if ui
        .add_enabled_ui(can, |ui| {
            widgets::wide_button(ui, Kind::Primary, Some(icons::SPLIT), &label)
        })
        .inner
        .clicked()
    {
        apply_cut(app, label);
    }
    ui.add_space(theme::SPACE_XS);
    if app.split_flow.joints.iter().any(|j| exists(app, j))
        && widgets::wide_button(ui, Kind::Secondary, None, &t!("splitw.next_connectors")).clicked()
    {
        set_step(app, Step::Connectors);
    }
    if widgets::wide_button(ui, Kind::Secondary, None, &t!("splitw.back_define")).clicked() {
        set_step(app, Step::Define);
    }
}

fn vec3(v: DVec3) -> String {
    format!("{},{},{}", v.x, v.y, v.z)
}

/// Step 2's Cut: `mesh.split_plane` or `mesh.split_box` on every selected
/// object, one command.
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

// --- Step 3: Connectors ---------------------------------------------------

/// A connector's parameters the panel edits, per kind: (name, label id).
fn editable(kind: &str) -> &'static [(&'static str, &'static str)] {
    match kind {
        "joint.dowel" => &[
            ("diameter", "splitw.param.diameter"),
            ("depth_on", "splitw.param.depth_on"),
            ("depth_other", "splitw.param.depth_other"),
            ("dowel_length", "splitw.param.dowel_length"),
            ("clearance", "splitw.param.clearance"),
        ],
        "joint.dovetail" => &[
            ("diameter", "splitw.param.width"),
            ("length", "splitw.param.length"),
            ("depth_on", "splitw.param.root"),
            ("depth_other", "splitw.param.depth_other"),
            ("clearance", "splitw.param.clearance"),
        ],
        "joint.insert" => &[
            ("diameter", "splitw.param.insert"),
            ("screw_diameter", "splitw.param.screw"),
            ("depth_on", "splitw.param.depth_on"),
            ("depth_other", "splitw.param.depth_other"),
            ("clearance", "splitw.param.clearance"),
        ],
        "joint.magnet" => &[
            ("diameter", "splitw.param.magnet"),
            ("depth_on", "splitw.param.depth_on"),
            ("depth_other", "splitw.param.depth_other"),
            ("clearance", "splitw.param.clearance"),
        ],
        _ => &[
            ("diameter", "splitw.param.diameter"),
            ("depth_on", "splitw.param.peg_root"),
            ("depth_other", "splitw.param.depth_other"),
            ("clearance", "splitw.param.clearance"),
        ],
    }
}

/// A placement's full parameters (its own and the spec's defaults), mm.
fn full_params(p: &Placement, gap_mm: f64) -> BTreeMap<String, f64> {
    mf_operations::placement_params(p, gap_mm)
        .map(|params| {
            params
                .0
                .iter()
                .filter_map(|(k, v)| match v {
                    mf_spec::ParamValue::Float(f) => Some((k.clone(), *f)),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_else(|_| p.params.clone()) // allow-swallow: an invalid placement's check card says why; the fields show what was set
}

/// Add a connector of the chosen kind at `at` on the open joint's solid
/// cut face, with the clearance the Fit class and Machine profile ask
/// for; it becomes the picked one.
pub fn add_connector(app: &mut MeshFixerApp, at: [f64; 2]) {
    let kind = app.split_flow.kind();
    let clearance = mf_operations::wanted_clearance(&settings(app));
    let f = &mut app.split_flow;
    let Some(joint) = f.joint.and_then(|j| f.joints.get_mut(j)) else {
        return;
    };
    let mut params = BTreeMap::from([(
        "clearance".to_owned(),
        (clearance * 1000.0).round() / 1000.0,
    )]);
    if kind == "joint.dovetail" {
        // Long enough to run out of the piece both ways, so the tail
        // slides in from the side.
        params.insert("groove_length".into(), 10_000.0);
    }
    joint.placements.push(Placement {
        connector: kind.to_owned(),
        on: joint.transparent.other(),
        at,
        angle: 0.0,
        params,
    });
    f.selected = Some(joint.placements.len() - 1);
}

fn connectors_panel(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    // The joints.
    section(ui, &t!("splitw.section.joints"));
    let rows: Vec<(usize, String, String)> = app
        .split_flow
        .joints
        .iter()
        .enumerate()
        .filter(|(_, j)| exists(app, j))
        .map(|(i, j)| {
            let status = if applied_now(app, j) {
                crate::tp!("splitw.joint.applied", j.placements.len())
            } else if j.placements.is_empty() {
                t!("splitw.joint.empty")
            } else {
                crate::tp!("splitw.joint.draft", j.placements.len())
            };
            (
                i,
                t!(
                    "splitw.joint.row",
                    a = name_of(app, j.a),
                    b = name_of(app, j.b),
                    label = j.label.clone()
                ),
                status,
            )
        })
        .collect();
    if rows.is_empty() {
        widgets::caption(ui, t!("splitw.joints.none"));
        ui.add_space(theme::SPACE_S);
        if widgets::wide_button(ui, Kind::Secondary, None, &t!("splitw.back_define")).clicked() {
            set_step(app, Step::Define);
        }
        return;
    }
    for (i, label, status) in rows {
        let on = app.split_flow.joint == Some(i);
        let r = widgets::button(
            ui,
            if on { Kind::Primary } else { Kind::Secondary },
            None,
            &label,
            ui.available_width(),
        );
        if r.clicked() {
            open_joint(app, i);
        }
        widgets::caption(ui, status);
    }
    let Some(joint) = app.split_flow.open_joint().cloned() else {
        widgets::caption(ui, t!("splitw.joints.pick"));
        return;
    };
    let j = app.split_flow.joint.unwrap_or(0);
    let (na, nb) = (name_of(app, joint.a), name_of(app, joint.b));
    // Which piece is see-through.
    section(ui, &t!("splitw.section.transparent"));
    let options = [
        (na.as_str(), na.as_str(), true),
        (nb.as_str(), nb.as_str(), true),
    ];
    let current = usize::from(joint.transparent == JointSide::B);
    if let (Some(i), _) = widgets::segmented(ui, &options, current) {
        let side = if i == 0 { JointSide::A } else { JointSide::B };
        if let Some(jt) = app.split_flow.joints.get_mut(j) {
            jt.transparent = side;
        }
    }
    let solid_name = if joint.solid() == JointSide::A {
        &na
    } else {
        &nb
    };
    let geometry = app.split_flow.geometry();
    match (&app.split_flow.geom, &geometry) {
        (GeomSlot::Failed(_, e), _) => {
            warning_card(ui, &t!("splitw.geometry_failed", reason = e.clone()));
            return;
        }
        (_, None) => {
            widgets::caption(ui, t!("splitw.reading"));
            return;
        }
        _ => {}
    }
    widgets::caption(ui, t!("splitw.place_help", name = solid_name.clone()));
    // The kind a click places.
    section(ui, &t!("splitw.section.kind"));
    let kinds = mf_parts::connectors::joint::JOINT_CONNECTORS;
    let labels: Vec<String> = kinds.iter().map(|k| kind_label(k)).collect();
    let options: Vec<(&str, &str, bool)> = labels
        .iter()
        .map(|l| (l.as_str(), l.as_str(), true))
        .collect();
    let current = kinds
        .iter()
        .position(|k| *k == app.split_flow.kind())
        .unwrap_or(0); // allow-swallow: kind is always one of JOINT_CONNECTORS
    if let (Some(i), _) = widgets::segmented(ui, &options, current) {
        app.split_flow.kind = Some(kinds[i]);
    }
    fit_row(app, ui);
    // The connectors placed.
    section(ui, &t!("splitw.section.placed"));
    let checks = app.split_flow.checks().unwrap_or_default();
    if joint.placements.is_empty() {
        widgets::caption(ui, t!("splitw.placed.none"));
    }
    for (i, p) in joint.placements.iter().enumerate() {
        let n = checks.get(i).map_or(0, Vec::len);
        let label = if n == 0 {
            t!(
                "splitw.placed.row_ok",
                n = i + 1,
                kind = kind_label(&p.connector)
            )
        } else {
            crate::tp!(
                "splitw.placed.row_warn",
                n,
                index = i + 1,
                kind = kind_label(&p.connector)
            )
        };
        let on = app.split_flow.selected == Some(i);
        if widgets::button(
            ui,
            if on { Kind::Primary } else { Kind::Secondary },
            if n == 0 {
                Some(icons::OK)
            } else {
                Some(icons::WARNING)
            },
            &label,
            ui.available_width(),
        )
        .clicked()
        {
            app.split_flow.selected = Some(i);
        }
    }
    if let Some(i) = app
        .split_flow
        .selected
        .filter(|&i| i < joint.placements.len())
    {
        selected_controls(app, ui, j, i, &joint, &checks);
    }
    // Every other connector's warnings too, each with its fix.
    for (i, list) in checks.iter().enumerate() {
        if Some(i) == app.split_flow.selected {
            continue;
        }
        for c in list {
            check_card(app, ui, j, i, c);
        }
    }
    // Apply.
    ui.add_space(theme::SPACE_L);
    let applied = applied_now(app, &joint);
    if applied {
        let later = app
            .session
            .records_to_current()
            .iter()
            .rev()
            .take_while(|r| Some(r.output_revision) != joint.applied.map(|a| a.1))
            .count();
        if later > 0 {
            widgets::caption(ui, crate::tp!("splitw.apply.later", later));
        } else {
            widgets::caption(ui, t!("splitw.apply.replaces"));
        }
    }
    let off_cap = checks.iter().flatten().any(|c| c.kind == CheckKind::OffCap);
    let can = app.busy.is_none() && !joint.placements.is_empty() && !off_cap;
    let label = if applied {
        t!("splitw.apply.again")
    } else {
        t!("splitw.apply")
    };
    if ui
        .add_enabled_ui(can, |ui| {
            widgets::wide_button(ui, Kind::Primary, Some(icons::CONNECTORS), &label)
        })
        .inner
        .clicked()
    {
        apply_connectors(app, j, label);
    }
}

/// The Fit class and Machine profile the clearance and the fit check use
/// (the Connectors tool's own choices, shared), and the minimum wall.
fn fit_row(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    section(ui, &t!("splitw.section.fit"));
    let (classes, class_err) = crate::connector_tool::fit_classes(app);
    let (profiles, profile_err) = crate::connector_tool::machine_profiles(app);
    if let Some(e) = class_err.or(profile_err) {
        widgets::caption(ui, t!("splitw.fit.unreadable", reason = e));
    }
    egui::ComboBox::from_id_salt("split fit class")
        .selected_text(t!(
            "splitw.fit.class",
            name = app.connectors.fit_class.clone()
        ))
        .width(ui.available_width())
        .show_ui(ui, |ui| {
            for c in &classes.classes {
                let label = format!("{} ({:+.2} mm)", c.name, c.clearance);
                ui.selectable_value(&mut app.connectors.fit_class, c.name.clone(), label);
            }
        });
    egui::ComboBox::from_id_salt("split machine profile")
        .selected_text(t!(
            "splitw.fit.machine",
            name = app.connectors.machine_profile.clone()
        ))
        .width(ui.available_width())
        .show_ui(ui, |ui| {
            for p in &profiles.profiles {
                ui.selectable_value(
                    &mut app.connectors.machine_profile,
                    p.name.clone(),
                    p.name.clone(),
                );
            }
        });
    ui.horizontal(|ui| {
        ui.label(RichText::new(t!("splitw.min_wall")).color(theme::TEXT_MUTED));
        let mut w = app.split_flow.min_wall();
        if ui
            .add(
                egui::DragValue::new(&mut w)
                    .speed(0.1)
                    .range(0.2..=20.0)
                    .suffix(" mm")
                    .max_decimals(2),
            )
            .changed()
        {
            app.split_flow.min_wall = Some(w);
        }
    });
}

fn selected_controls(
    app: &mut MeshFixerApp,
    ui: &mut egui::Ui,
    j: usize,
    i: usize,
    joint: &Joint,
    checks: &[Vec<Check>],
) {
    let p = joint.placements[i].clone();
    let gap_mm = joint.frame.gap / mm(app);
    let full = full_params(&p, gap_mm);
    widgets::card(ui, |ui| {
        ui.label(
            RichText::new(t!(
                "splitw.selected",
                n = i + 1,
                kind = kind_label(&p.connector)
            ))
            .color(theme::TEXT_STRONG),
        );
        let mut edited = p.clone();
        let mut changed = false;
        for (name, label) in editable(&p.connector) {
            let mut v = full.get(*name).copied().unwrap_or(0.0); // allow-swallow: every editable name is in its connector's spec, so validation always fills it
            if *name == "dowel_length" && v == 0.0 {
                v = mf_operations::placement_params(&p, gap_mm)
                    .ok()
                    .and_then(|params| mf_parts::connectors::joint::dowel_length(&params).ok())
                    .unwrap_or(0.0); // allow-swallow: shows 0 (automatic) if the placement is invalid; its check says why
            }
            ui.horizontal(|ui| {
                ui.label(RichText::new(crate::i18n::tr(label, &[])).color(theme::TEXT_MUTED));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let r = ui.add(
                        egui::DragValue::new(&mut v)
                            .speed(0.05)
                            .range(if *name == "clearance" {
                                -2.0..=5.0
                            } else {
                                0.0..=1000.0
                            })
                            .suffix(" mm")
                            .max_decimals(2),
                    );
                    let a11y = crate::i18n::tr(label, &[]);
                    r.widget_info(|| {
                        egui::WidgetInfo::labeled(egui::Role::SpinButton, true, &a11y)
                    });
                    if r.changed() {
                        edited.params.insert((*name).to_owned(), v);
                        changed = true;
                    }
                });
            });
        }
        if p.connector == "joint.dovetail" {
            ui.horizontal(|ui| {
                ui.label(RichText::new(t!("splitw.param.angle")).color(theme::TEXT_MUTED));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let r = ui.add(
                        egui::DragValue::new(&mut edited.angle)
                            .speed(1.0)
                            .range(-180.0..=180.0)
                            .suffix("°"),
                    );
                    changed |= r.changed();
                });
            });
        }
        if changed && let Some(jt) = app.split_flow.joints.get_mut(j) {
            jt.placements[i] = edited;
        }
        ui.add_space(theme::SPACE_XS);
        if widgets::wide_button(
            ui,
            Kind::Secondary,
            Some(icons::CLOSE),
            &t!("splitw.delete"),
        )
        .clicked()
        {
            if let Some(jt) = app.split_flow.joints.get_mut(j) {
                jt.placements.remove(i);
            }
            app.split_flow.selected = None;
        }
    });
    for c in checks.get(i).into_iter().flatten() {
        check_card(app, ui, j, i, c);
    }
}

/// One check's warning, with its one-click fix.
fn check_card(app: &mut MeshFixerApp, ui: &mut egui::Ui, j: usize, i: usize, c: &Check) {
    let mut fix = false;
    widgets::card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(icons::WARNING)
                    .font(icons::font(theme::ICON_SIZE))
                    .color(theme::LOOSE_PIECE),
            );
            ui.add(
                egui::Label::new(
                    RichText::new(t!("splitw.check", n = i + 1, message = c.message.clone()))
                        .color(theme::TEXT),
                )
                .wrap(),
            );
        });
        let label = match c.kind {
            CheckKind::Breakout(_) => t!("splitw.fix.breakout"),
            CheckKind::EdgeDistance | CheckKind::OffCap => t!("splitw.fix.inwards"),
            CheckKind::DowelLength => t!("splitw.fix.dowel"),
            CheckKind::Fit => t!("splitw.fix.fit"),
        };
        fix = widgets::wide_button(ui, Kind::Secondary, None, &label).clicked();
    });
    ui.add_space(theme::SPACE_XS);
    if fix {
        apply_fix(app, j, i, c.kind);
    }
}

/// Apply check `kind`'s one-click fix to connector `i` of joint `j`.
pub fn apply_fix(app: &mut MeshFixerApp, j: usize, i: usize, kind: CheckKind) {
    let (Some(joint), Some(g)) = (
        app.split_flow.joints.get(j).cloned(),
        app.split_flow.geometry(),
    ) else {
        return;
    };
    let Some(p) = joint.placements.get(i).cloned() else {
        return;
    };
    let s = settings(app);
    let ctx = mf_runtime::RunContext::none();
    let names = [name_of(app, joint.a), name_of(app, joint.b)];
    let fixed = {
        let pc = pieces(&names, &g);
        mf_operations::fix(&joint.frame, [&pc[0], &pc[1]], &p, kind, &s, &ctx)
    };
    match fixed {
        Ok(Some(q)) => {
            if let Some(jt) = app.split_flow.joints.get_mut(j) {
                jt.placements[i] = q;
            }
            app.split_flow.selected = Some(i);
        }
        Ok(None) => app.status = Some(t!("splitw.fix.none")),
        Err(e) => app.status = Some(t!("splitw.fix.failed", reason = e.to_string())),
    }
}

/// Step 3's Apply: `split.place_connectors` with every connector on the
/// joint, one command. A joint whose connectors are already applied is
/// applied again from the revision before them.
fn apply_connectors(app: &mut MeshFixerApp, j: usize, label: String) {
    let Some(joint) = app.split_flow.joints.get(j).cloned() else {
        return;
    };
    if applied_now(app, &joint)
        && let Some((before, _)) = joint.applied
    {
        app.session.checkout(before);
    }
    let s = settings(app);
    let (profiles, _) = crate::connector_tool::machine_profiles(app);
    let plate = profiles.get(&app.connectors.machine_profile).map_or(
        mf_parts::MachineProfile::uncalibrated().print_volume_mm,
        |p| p.print_volume_mm,
    );
    let connectors = serde_json::to_string(&joint.placements).unwrap_or_default(); // allow-swallow: Placement is plain data; serializing it can't fail
    let params = BTreeMap::from([
        ("plane_point".to_owned(), vec3(joint.frame.point)),
        ("plane_normal".to_owned(), vec3(joint.frame.normal)),
        ("plane_u".to_owned(), vec3(joint.frame.u)),
        ("gap".to_owned(), joint.frame.gap.to_string()),
        ("connectors".to_owned(), connectors),
        ("min_wall".to_owned(), s.min_wall.to_string()),
        (
            "fit_clearance".to_owned(),
            s.fit_class.clearance.to_string(),
        ),
        (
            "machine_tolerance".to_owned(),
            s.machine.hole_undersize.to_string(),
        ),
        ("plate_width".to_owned(), plate[0].to_string()),
        ("plate_depth".to_owned(), plate[1].to_string()),
    ]);
    app.refusal = None;
    app.split_flow.pending_apply = Some(j);
    app.run_invocation(
        Invocation {
            op_id: "split.place_connectors".into(),
            target: joint.a,
            selection: None,
            operands: vec![("side_b".to_owned(), joint.b, None)],
            raw_params: params,
        },
        label,
    );
    if app.busy.is_none() {
        app.split_flow.pending_apply = None;
    }
}

// ---------------------------------------------------------------------
// The view: the box's gizmo and face handles, step 3's placing.
// ---------------------------------------------------------------------

/// The box's gizmo (move only) in step 1's Print-volume mode.
pub fn box_gizmo(app: &MeshFixerApp) -> Option<(Gizmo, Option<Handle>)> {
    let f = &app.split_flow;
    if f.step != Step::Define || f.mode != CutMode::Volume {
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

fn view(app: &MeshFixerApp) -> Option<(mf_render::OrbitCamera, (f64, f64))> {
    let cam = app.camera?;
    let rect = app.viewport.last_rect?;
    Some((cam, (rect.width() as f64, rect.height() as f64)))
}

fn screen(cam: &mf_render::OrbitCamera, size: (f64, f64), p: DVec3) -> Option<(f64, f64)> {
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

/// The press is Split's own: a gizmo handle, a box face handle, or (step
/// 3) the open joint's solid cut face, a connector or its handles.
pub fn claims(app: &MeshFixerApp) -> Option<Claims> {
    if app.busy.is_some() {
        return None;
    }
    let f = &app.split_flow;
    match f.step {
        Step::Define if f.mode == CutMode::Volume => {
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
        }
        Step::Define | Step::Cut => {
            let (g, _) = crate::shape_tools::split_gizmo(app)?;
            let (cam, size) = view(app)?;
            Some(Box::new(move |px: (f64, f64)| g.hit(&cam, size, px).is_some()) as Claims)
        }
        Step::Connectors => {
            let state = PlaceView::of(app)?;
            Some(Box::new(move |px: (f64, f64)| state.hit(px).is_some()) as Claims)
        }
    }
}

/// Split's input in the view, by step.
pub fn input(app: &mut MeshFixerApp, input: &crate::shell::ToolInput) -> Input {
    match app.split_flow.step {
        Step::Define if app.split_flow.mode == CutMode::Volume => volume_input(app, input),
        Step::Define | Step::Cut => crate::shape_tools::plane_input(app, input),
        Step::Connectors => connector_input(app, input),
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
                    v.preset = t!("splitw.preset.custom");
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

/// What step 3's input and drawing need of a frame, copied out so the
/// view's claim test can run without the app.
#[derive(Clone)]
struct PlaceView {
    cam: mf_render::OrbitCamera,
    size: (f64, f64),
    frame: JointFrame,
    solid: JointSide,
    cap: CapRegion,
    /// Each connector's centre and reach on the cap (2D), and its handles'
    /// screen positions.
    connectors: Vec<([f64; 2], f64)>,
    selected: Option<usize>,
    handles: Option<((f64, f64), (f64, f64))>,
    /// The other joints: their frames and where their pieces overlap
    /// (world bounds), to pick one by clicking its cut face.
    others: Vec<(usize, JointFrame, DVec3, DVec3)>,
}

impl PlaceView {
    fn of(app: &MeshFixerApp) -> Option<PlaceView> {
        let f = &app.split_flow;
        if f.step != Step::Connectors {
            return None;
        }
        let joint = f.open_joint()?;
        let g = f.geometry()?;
        let (cam, size) = view(app)?;
        let others = f
            .joints
            .iter()
            .enumerate()
            .filter(|(i, j)| Some(*i) != f.joint && exists(app, j))
            .filter_map(|(i, j)| {
                let (a, b) = (app.object_world_bounds(j.a)?, app.object_world_bounds(j.b)?);
                let lo = DVec3::from(a.min).max(DVec3::from(b.min));
                let hi = DVec3::from(a.max).min(DVec3::from(b.max));
                let pad = DVec3::splat(j.frame.gap + (hi - lo).abs().max_element() * 1e-3 + 1e-6);
                Some((i, j.frame, lo - pad, hi + pad))
            })
            .collect();
        let solid = joint.solid();
        let cap = g.caps[match solid {
            JointSide::A => 0,
            JointSide::B => 1,
        }]
        .clone();
        let scale = mm(app);
        let gap_mm = joint.frame.gap / scale;
        let connectors: Vec<([f64; 2], f64)> = joint
            .placements
            .iter()
            .map(|p| {
                let reach = mf_operations::placement_params(p, gap_mm)
                    .ok()
                    .and_then(|params| mf_operations::footprint_2d(p, &params, scale).ok())
                    .map(|pts| {
                        pts.iter()
                            .map(|q| ((q[0] - p.at[0]).powi(2) + (q[1] - p.at[1]).powi(2)).sqrt())
                            .fold(0.0, f64::max)
                    })
                    .unwrap_or(scale); // allow-swallow: an invalid placement is still pickable at a 1 mm reach; its check card says why
                (p.at, reach)
            })
            .collect();
        let selected = f.selected.filter(|&i| i < connectors.len());
        let handles = selected.and_then(|i| {
            let (at, reach) = connectors[i];
            let p = joint.placements[i].angle.to_radians();
            let rim = [at[0] + reach * p.cos(), at[1] + reach * p.sin()];
            let w = joint.frame.at(rim) + (joint.frame.cap_point(solid) - joint.frame.point);
            let s = screen(&cam, size, w)?;
            Some((s, (s.0 + 16.0, s.1)))
        });
        Some(PlaceView {
            cam,
            size,
            frame: joint.frame,
            solid,
            cap,
            connectors,
            selected,
            handles,
            others,
        })
    }

    /// The pointer on the solid cut face's plane (2D), if it meets it.
    fn on_plane(&self, px: (f64, f64)) -> Option<[f64; 2]> {
        let (o, d) = self.cam.ray(self.cam.target, self.size, px);
        let (o, d) = (DVec3::from(o), DVec3::from(d));
        let p0 = self.frame.cap_point(self.solid);
        let n = self.frame.normal;
        let denom = d.dot(n);
        if denom.abs() < 1e-9 {
            return None;
        }
        let t = (p0 - o).dot(n) / denom;
        (t > 0.0).then(|| self.frame.to_2d(o + d * t))
    }

    fn hit(&self, px: (f64, f64)) -> Option<Hit> {
        let near = |s: (f64, f64)| ((s.0 - px.0).powi(2) + (s.1 - px.1).powi(2)).sqrt() < 8.0;
        if let (Some(i), Some((size, depth))) = (self.selected, self.handles) {
            if near(depth) {
                return Some(Hit::Depth(i));
            }
            if near(size) {
                return Some(Hit::Size(i));
            }
        }
        let Some(uv) = self.on_plane(px).filter(|uv| {
            self.cap.contains(*uv) || {
                self.connectors.iter().any(|(at, r)| {
                    ((uv[0] - at[0]).powi(2) + (uv[1] - at[1]).powi(2)).sqrt() <= r * 1.1
                })
            }
        }) else {
            return self.other_joint(px);
        };
        // The picked one first, then the nearest whose outline holds it.
        let inside = |(i, (at, reach)): (usize, &([f64; 2], f64))| {
            let d = ((uv[0] - at[0]).powi(2) + (uv[1] - at[1]).powi(2)).sqrt();
            (d <= *reach * 1.1).then_some((i, d))
        };
        if let Some(i) = self.selected
            && inside((i, &self.connectors[i])).is_some()
        {
            return Some(Hit::Connector(i));
        }
        if let Some((i, _)) = self
            .connectors
            .iter()
            .enumerate()
            .filter_map(inside)
            .min_by(|a, b| a.1.total_cmp(&b.1))
        {
            return Some(Hit::Connector(i));
        }
        self.cap.contains(uv).then_some(Hit::Cap(uv))
    }

    /// Another joint's cut face under `px` (the nearest along the ray):
    /// where the ray meets its plane inside both its pieces' bounds.
    fn other_joint(&self, px: (f64, f64)) -> Option<Hit> {
        let (o, d) = self.cam.ray(self.cam.target, self.size, px);
        let (o, d) = (DVec3::from(o), DVec3::from(d));
        self.others
            .iter()
            .filter_map(|(i, frame, lo, hi)| {
                let denom = d.dot(frame.normal);
                if denom.abs() < 1e-9 {
                    return None;
                }
                let t = (frame.point - o).dot(frame.normal) / denom;
                let p = o + d * t;
                (t > 0.0 && p.cmpge(*lo).all() && p.cmple(*hi).all()).then_some((*i, t))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| Hit::Joint(i))
    }
}

fn connector_input(app: &mut MeshFixerApp, input: &crate::shell::ToolInput) -> Input {
    use crate::viewport::ToolPointer;
    let Some(state) = PlaceView::of(app) else {
        app.split_flow.hover = None;
        return Input::Pass;
    };
    let j = app.split_flow.joint.unwrap_or(0);
    let scale = mm(app);
    match input.pointer {
        Some(ToolPointer::Start { from, at: _ }) => {
            app.split_flow.drag = match state.hit(from) {
                Some(Hit::Size(i)) => Some(Drag::Size { index: i }),
                Some(Hit::Depth(i)) => {
                    let p = &app.split_flow.joints[j].placements[i];
                    full_params(p, app.split_flow.joints[j].frame.gap / scale)
                        .get("depth_on")
                        .map(|&depth| Drag::Depth {
                            index: i,
                            from_y: from.1,
                            depth,
                        })
                }
                Some(Hit::Connector(i)) => {
                    app.split_flow.selected = Some(i);
                    state.on_plane(from).map(|uv| {
                        let at = state.connectors[i].0;
                        Drag::Move {
                            index: i,
                            grab: [uv[0] - at[0], uv[1] - at[1]],
                        }
                    })
                }
                _ => None,
            };
        }
        Some(ToolPointer::Move(px)) | Some(ToolPointer::End(px)) => {
            let drag = app.split_flow.drag;
            let joint = &mut app.split_flow.joints[j];
            match drag {
                Some(Drag::Move { index, grab }) => {
                    if let (Some(uv), Some(p)) =
                        (state.on_plane(px), joint.placements.get_mut(index))
                    {
                        p.at = [uv[0] - grab[0], uv[1] - grab[1]];
                    }
                }
                Some(Drag::Size { index }) => {
                    if let (Some(uv), Some(p)) =
                        (state.on_plane(px), joint.placements.get_mut(index))
                    {
                        let r = ((uv[0] - p.at[0]).powi(2) + (uv[1] - p.at[1]).powi(2)).sqrt();
                        let mut value = (2.0 * r / scale).clamp(0.5, 200.0);
                        if p.connector == "joint.dovetail" {
                            // The rim handle is the tail's end: its length.
                            p.params
                                .insert("length".into(), (value * 10.0).round() / 10.0);
                        } else {
                            if p.connector == "joint.magnet" {
                                value -= p.param("clearance").unwrap_or(0.0); // allow-swallow: add_connector always sets clearance
                            }
                            p.params
                                .insert("diameter".into(), (value.max(0.5) * 10.0).round() / 10.0);
                        }
                    }
                }
                Some(Drag::Depth {
                    index,
                    from_y,
                    depth,
                }) => {
                    let (cam, size) = (state.cam, state.size);
                    // Document units per point at the target, in mm.
                    let per_pt =
                        2.0 * cam.distance * (cam.fov_y / 2.0).tan() / size.1.max(1.0) / scale;
                    if let Some(p) = joint.placements.get_mut(index) {
                        let d = (depth + (from_y - px.1) * per_pt).clamp(0.5, 500.0);
                        p.params
                            .insert("depth_on".into(), (d * 10.0).round() / 10.0);
                    }
                }
                None => {}
            }
            if matches!(input.pointer, Some(ToolPointer::End(_))) {
                app.split_flow.drag = None;
            }
        }
        Some(ToolPointer::Click(px)) => match state.hit(px) {
            Some(Hit::Connector(i) | Hit::Size(i) | Hit::Depth(i)) => {
                app.split_flow.selected = Some(i);
            }
            Some(Hit::Cap(uv)) => add_connector(app, uv),
            Some(Hit::Joint(i)) => open_joint(app, i),
            None => {}
        },
        None => {}
    }
    app.split_flow.hover = if app.split_flow.drag.is_some() {
        None
    } else {
        input.hover.and_then(|px| state.hit(px))
    };
    Input::Consumed
}

// ---------------------------------------------------------------------
// Drawing over the view.
// ---------------------------------------------------------------------

/// The box (Print volume) or the open joint's connectors, over the view.
pub fn overlay(app: &MeshFixerApp, painter: &egui::Painter, rect: egui::Rect) {
    let Some((cam, size)) = view(app) else { return };
    let at = |p: DVec3| {
        screen(&cam, size, p).map(|(x, y)| rect.left_top() + egui::vec2(x as f32, y as f32))
    };
    let f = &app.split_flow;
    if f.mode == CutMode::Volume && matches!(f.step, Step::Define | Step::Cut) {
        draw_box(app, painter, &cam, &at);
    }
    if f.step == Step::Connectors {
        draw_connectors(app, painter, &at);
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

fn draw_connectors(
    app: &MeshFixerApp,
    painter: &egui::Painter,
    at: &dyn Fn(DVec3) -> Option<egui::Pos2>,
) {
    let f = &app.split_flow;
    let (Some(joint), Some(g)) = (f.open_joint(), f.geometry()) else {
        return;
    };
    let solid = joint.solid();
    let scale = mm(app);
    // The solid cut face's outline.
    let lift = joint.frame.cap_point(solid) - joint.frame.point;
    let cap = &g.caps[match solid {
        JointSide::A => 0,
        JointSide::B => 1,
    }];
    for s in &cap.boundary {
        if let (Some(p), Some(q)) = (
            at(joint.frame.at(s[0]) + lift),
            at(joint.frame.at(s[1]) + lift),
        ) {
            painter.line_segment(
                [p, q],
                egui::Stroke::new(1.5, theme::ACCENT.gamma_multiply(0.8)),
            );
        }
    }
    let checks = f.checks().unwrap_or_default();
    let previews = f
        .previews
        .as_ref()
        .map(|(_, _, p)| p.clone())
        .unwrap_or_default();
    let counterpart = theme::SELECTED;
    for (i, p) in joint.placements.iter().enumerate() {
        let selected = f.selected == Some(i);
        let hovered = matches!(f.hover, Some(Hit::Connector(k)) if k == i);
        let warn = checks.get(i).is_some_and(|c| !c.is_empty());
        let own = if warn {
            theme::LOOSE_PIECE
        } else {
            theme::RECENT_REPAIR
        };
        // The parts: its own (into the solid piece, behind the face:
        // faint), the counterpart (into the see-through piece), the
        // loose dowel.
        for part in previews.get(i).into_iter().flatten() {
            let (colour, width) = match part.side {
                Some(s) if s == solid => (own.gamma_multiply(0.45), 1.0),
                Some(_) => (counterpart, if selected { 2.0 } else { 1.4 }),
                None => (theme::TEXT_MUTED.gamma_multiply(0.7), 1.0),
            };
            for e in &part.edges {
                if let (Some(a), Some(b)) = (at(e[0]), at(e[1])) {
                    painter.line_segment([a, b], egui::Stroke::new(width, colour));
                }
            }
        }
        // Its outline on the solid face, filled.
        let outline = mf_operations::placement_params(p, joint.frame.gap / scale)
            .ok()
            .and_then(|params| mf_operations::footprint_2d(p, &params, scale).ok());
        if let Some(outline) = outline {
            let pts: Option<Vec<egui::Pos2>> = outline
                .iter()
                .map(|q| at(joint.frame.at(*q) + lift))
                .collect();
            if let Some(pts) = pts {
                let fill = own.gamma_multiply(if selected || hovered { 0.55 } else { 0.35 });
                painter.add(egui::Shape::convex_polygon(
                    pts.clone(),
                    fill,
                    egui::Stroke::NONE,
                ));
                painter.add(egui::Shape::closed_line(
                    pts,
                    egui::Stroke::new(
                        if selected { 2.5 } else { 1.5 },
                        if selected { theme::HOVER } else { own },
                    ),
                ));
            }
        }
        if let Some(c) = at(joint.frame.at(p.at) + lift) {
            painter.text(
                c,
                egui::Align2::CENTER_CENTER,
                format!("{}", i + 1),
                egui::FontId::proportional(11.0),
                theme::TEXT_STRONG,
            );
        }
    }
    // The picked one's handles: its rim (size) and depth.
    if let Some(state) = PlaceView::of(app)
        && let Some((size, depth)) = state.handles
    {
        let origin = app
            .viewport
            .last_rect
            .map_or(egui::Pos2::ZERO, |r| r.left_top());
        let pos = |s: (f64, f64)| origin + egui::vec2(s.0 as f32, s.1 as f32);
        let hot = |h: Hit| f.hover == Some(h);
        let i = state.selected.unwrap_or(0);
        painter.circle(
            pos(size),
            if hot(Hit::Size(i)) { 7.0 } else { 5.5 },
            theme::HOVER,
            egui::Stroke::new(1.5, theme::VIEWPORT_BACKGROUND),
        );
        let r = if hot(Hit::Depth(i)) { 7.0 } else { 5.5 };
        painter.rect_filled(
            egui::Rect::from_center_size(pos(depth), egui::vec2(r * 2.0, r * 2.0)),
            1.5,
            theme::SELECTED,
        );
        painter.text(
            pos(depth) + egui::vec2(10.0, 0.0),
            egui::Align2::LEFT_CENTER,
            t!("splitw.handle.depth"),
            egui::FontId::proportional(11.0),
            theme::TEXT,
        );
    }
}
