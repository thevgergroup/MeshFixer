//! The Transform tool (MF-OBJ-001, Phase 1 Task 5): the gizmo in the 3D
//! view, numeric fields, Drop to plate and Reset.
//!
//! Everything here is small `f64` arithmetic on the UI thread: a drag
//! changes only the dragged object's model matrix (a uniform), and its
//! release commits one `object.set_transform` with the absolute resulting
//! matrix (D7). Nothing reads the mesh: the pivot is the object's
//! bounding-box centre, from bounds the analysis worker measured, and the
//! plate snap's lowest point is found on a worker too.

use eframe::egui;
use glam::{DMat3, DVec3};
use mf_command::ObjectId;
use mf_geometry::Mesh;
use mf_render::{OrbitCamera, PlateSnap, Snap};
use mf_spec::ParamValue;

use crate::app::FollowUp;
use crate::viewport::ToolPointer;
use mf_io::threemf::Unit;
use mf_render::{Decomposed, Drag, Gizmo, GizmoMode, Handle, compose, decompose};

use crate::MeshFixerApp;

/// The tool's rail id.
pub const ID: &str = "transform";

/// The keys it takes while open (no modifiers): W, E and R pick Move,
/// Rotate and Scale, as in Meshmixer.
pub const KEYS: &[(egui::Key, crate::tools::ToolKey)] = &[
    (egui::Key::W, crate::tools::ToolKey::Mode(GizmoMode::Move)),
    (egui::Key::E, crate::tools::ToolKey::Mode(GizmoMode::Rotate)),
    (egui::Key::R, crate::tools::ToolKey::Mode(GizmoMode::Scale)),
];

/// The gizmo's size on screen (points).
pub const GIZMO_SIZE_PT: f64 = 110.0;

/// What the "Transform includes shear" note says (Review Focus 2).
pub const SHEARED_NOTE: &str = "Transform includes shear (from the file)";
/// What a mirrored object's note says.
pub const MIRRORED_NOTE: &str = "Mirrored";

/// One numeric field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Field {
    Position(usize),
    Rotation(usize),
    Scale(usize),
}

impl Field {
    /// Its accessible name ("Position X", "Rotation Z", "Scale Y").
    pub fn label(self) -> String {
        let (what, axis) = match self {
            Field::Position(a) => ("Position", a),
            Field::Rotation(a) => ("Rotation", a),
            Field::Scale(a) => ("Scale", a),
        };
        format!("{what} {}", AXIS_NAMES[axis])
    }
}

pub const AXIS_NAMES: [&str; 3] = ["X", "Y", "Z"];

/// A gizmo drag under way.
#[derive(Debug, Clone, Copy)]
pub struct Dragging {
    pub object: ObjectId,
    pub drag: Drag,
    pub start: [f64; 12],
    /// The transform the pointer gives now (drawn, not yet committed).
    pub current: [f64; 12],
}

/// Transform's session (ADR-0007): the open tool's gizmo mode, drag and
/// fields, made when it opens and dropped when it closes.
#[derive(Debug, Default)]
pub struct TransformSession {
    pub tool: Option<TransformTool>,
}

impl crate::session::ToolSession for TransformSession {
    /// A drag draws the object where the pointer puts it.
    fn preview(&self, _app: &MeshFixerApp) -> crate::session::PreviewSpec {
        match self.tool.as_ref().and_then(|t| t.dragging) {
            Some(d) => crate::session::PreviewSpec::Placement(vec![(d.object, d.current)]),
            None => crate::session::PreviewSpec::None,
        }
    }

    fn cancel_drag(&mut self, _app: &mut MeshFixerApp) -> bool {
        self.tool.as_mut().and_then(|t| t.dragging.take()).is_some()
    }

    fn close(&mut self, _app: &mut MeshFixerApp) {
        self.tool = None;
    }

    fn held(&self, _app: &MeshFixerApp) -> Vec<&'static str> {
        let Some(t) = &self.tool else {
            return Vec::new();
        };
        let mut held = vec!["the gizmo"];
        if t.dragging.is_some() {
            held.push("a gizmo drag");
        }
        if t.editing.is_some() {
            held.push("a field being typed in");
        }
        held
    }
}

/// The open Transform tool.
#[derive(Debug)]
pub struct TransformTool {
    pub mode: GizmoMode,
    /// Scale fields move together.
    pub uniform: bool,
    /// The handle under the pointer.
    pub hover: Option<Handle>,
    pub dragging: Option<Dragging>,
    /// The numeric field being typed in, and its text.
    pub editing: Option<(Field, String)>,
}

impl Default for TransformTool {
    fn default() -> Self {
        TransformTool {
            mode: GizmoMode::Move,
            uniform: true,
            hover: None,
            dragging: None,
            editing: None,
        }
    }
}

/// The object's placement as the fields show it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    /// The bounding-box centre in world space (document units).
    pub position: DVec3,
    /// `None`: the transform has shear, so it has no rotation and scale.
    pub rotation_deg: Option<DVec3>,
    /// Scale factors (1 = 100%), positive; `mirrored` says if one is
    /// negative.
    pub scale: Option<DVec3>,
    pub mirrored: bool,
}

/// The linear part of `t` and its translation, as `model_matrix` uses
/// them (each column one of `t`'s row-triples).
fn parts(t: &[f64; 12]) -> (DMat3, DVec3) {
    (
        DMat3::from_cols(
            DVec3::new(t[0], t[1], t[2]),
            DVec3::new(t[3], t[4], t[5]),
            DVec3::new(t[6], t[7], t[8]),
        ),
        DVec3::new(t[9], t[10], t[11]),
    )
}

fn rows(linear: DMat3, translate: DVec3) -> [f64; 12] {
    let (a, b, c) = (linear.col(0), linear.col(1), linear.col(2));
    [
        a.x,
        a.y,
        a.z,
        b.x,
        b.y,
        b.z,
        c.x,
        c.y,
        c.z,
        translate.x,
        translate.y,
        translate.z,
    ]
}

/// Where object-space point `local` lands under `t`.
pub fn world_point(t: &[f64; 12], local: DVec3) -> DVec3 {
    let (l, tr) = parts(t);
    l * local + tr
}

/// The placement the fields show for transform `t` of an object whose
/// bounding-box centre is `centre` (object space).
pub fn placement(t: &[f64; 12], centre: DVec3) -> Placement {
    let position = world_point(t, centre);
    match decompose(t) {
        Decomposed::Trs {
            rotate_deg_xyz,
            scale,
            ..
        } => Placement {
            position,
            rotation_deg: Some(rotate_deg_xyz),
            scale: Some(scale.abs()),
            mirrored: scale.x < 0.0,
        },
        Decomposed::Sheared => {
            let (l, _) = parts(t);
            Placement {
                position,
                rotation_deg: None,
                scale: None,
                mirrored: l.determinant() < 0.0,
            }
        }
    }
}

/// `t` moved so the bounding-box centre lands at `position`.
pub fn with_position(t: &[f64; 12], centre: DVec3, position: DVec3) -> [f64; 12] {
    let (l, tr) = parts(t);
    rows(l, tr + position - world_point(t, centre))
}

/// `t` with this rotation and scale (`scale` positive; a mirrored object
/// stays mirrored), about the bounding-box centre, which stays put.
/// `None` for a sheared transform (its fields are read-only).
pub fn with_rotation_scale(
    t: &[f64; 12],
    centre: DVec3,
    rotation_deg: DVec3,
    scale: DVec3,
) -> Option<[f64; 12]> {
    let p = placement(t, centre);
    p.rotation_deg?;
    let signed = if p.mirrored {
        DVec3::new(-scale.x, scale.y, scale.z)
    } else {
        scale
    };
    let (l, _) = parts(&compose(DVec3::ZERO, rotation_deg, signed, DVec3::ZERO));
    Some(rows(l, p.position - l * centre))
}

/// A field's value as it is shown and edited: up to 4 decimals, no
/// trailing zeros.
pub fn field_text(v: f64) -> String {
    let s = format!("{v:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
}

/// The gizmo for an object whose bounding-box centre sits at `pivot`.
pub fn gizmo(pivot: DVec3, mode: GizmoMode) -> Gizmo {
    Gizmo {
        pivot,
        mode,
        size_pt: GIZMO_SIZE_PT,
    }
}

use crate::viewport::MIN_SNAP_PX;

/// The one shared snap step every gizmo tool calls (ADR-0006 rule 3: "no
/// tool should have its own snap constant") — the Transform gizmo, Split's
/// plane and print-volume box, and any future gizmo tool, all call this
/// instead of keeping their own grid number. A 1-2-5-mm step, picked so
/// it is at least [`MIN_SNAP_PX`] on screen at `cam`'s current zoom
/// (measured at `pivot`), returned in document units alongside the mm
/// value itself (for the tool hint, "snap 1 mm"). Replaces a fixed step
/// in document units (`mf_render::Snap::default`'s old `grid: Some(1.0)`),
/// which snapped every drag on a metre-scale document to zero, because
/// "1 document unit" was 1000 mm there (the owner's stanford-bunny
/// report, MF-OBJ-002) — "a literal length constant in document units is
/// a bug" (ADR-0006 rule 3).
pub fn snap_step(
    cam: &mf_render::OrbitCamera,
    pivot: DVec3,
    view: (f64, f64),
    unit: Unit,
) -> (f64, f64) {
    let mm = mf_units::Length::new(1.0, unit).mm();
    let step_mm = mf_render::adaptive_grid_mm(cam, pivot, view, mm, MIN_SNAP_PX);
    (step_mm / mm, step_mm)
}

/// The angle snap every gizmo uses, degrees (not a length).
pub const ANGLE_SNAP_DEG: f64 = 15.0;
/// The scale snap every gizmo uses, a fraction (not a length).
pub const SCALE_SNAP_STEP: f64 = 0.1;

/// A gizmo snap on a `grid` step in document units (from [`snap_step`]),
/// with the shared angle and scale steps and no plate snap: what Split's
/// plane and print-volume box gizmos drag with. Spelled out rather than
/// `..Snap::default()`, whose grid is a fixed document-unit step
/// (ADR-0006 rule 3).
pub fn grid_snap(grid: f64) -> mf_render::Snap {
    mf_render::Snap {
        grid: Some(grid),
        angle_deg: Some(ANGLE_SNAP_DEG),
        scale_step: Some(SCALE_SNAP_STEP),
        plate: None,
    }
}

/// The tool hint's snap clause ("snap 1 mm", "snap 0.5 mm"): `step_mm` as
/// [`field_text`] gives it, so a sub-mm step (a micron-scale document)
/// reads with no trailing zeros either.
pub fn snap_hint(step_mm: f64) -> String {
    format!("snap {} mm", field_text(step_mm))
}

/// The Transform tool (MF-OBJ-001, Phase 1 Task 5): its gizmo, its
/// keys and its commands, on the app (they move objects, so they need
/// it), with the tool's own state in its session.
impl MeshFixerApp {
    /// The open Transform tool, if it is.
    pub fn transform_tool(&self) -> Option<&TransformTool> {
        self.sessions.find::<TransformSession>()?.tool.as_ref()
    }

    pub fn transform_tool_mut(&mut self) -> Option<&mut TransformTool> {
        self.sessions.find_mut::<TransformSession>()?.tool.as_mut()
    }

    /// Open the Transform tool on the active object (T): the whole object
    /// becomes the target.
    pub fn open_transform(&mut self) {
        if self.active_mesh().is_none() {
            return;
        }
        self.clear_selection();
        if self.transform_tool().is_none() {
            self.open_rail_tool(ID);
        }
    }

    /// Transform's own state, when it opens (its rail `on_open`): it works
    /// on an object, so with none it doesn't open.
    pub(crate) fn start_transform(&mut self) {
        if self.active_mesh().is_none() {
            self.active_tool = crate::shell::SELECT;
            return;
        }
        if let Some(s) = self.sessions.find_mut::<TransformSession>() {
            s.tool = Some(TransformTool::default());
        }
    }

    /// Switch the gizmo's mode. A sheared transform only moves: its
    /// rotation and scale can't be told apart from the shear.
    pub fn set_transform_mode(&mut self, mode: GizmoMode) {
        let sheared = self
            .active_placement()
            .is_some_and(|p| p.rotation_deg.is_none());
        if self.transform_tool().is_none() {
            return;
        }
        if sheared && mode != GizmoMode::Move {
            self.status = Some(format!(
                "{SHEARED_NOTE}: only Move works on it. Rotate and Scale would mix with the shear."
            ));
            return;
        }
        if let Some(t) = self.transform_tool_mut() {
            t.mode = mode;
            t.dragging = None;
        }
    }

    /// Transform works on a whole object: a defect targeted (from the
    /// tree) or the object gone closes it.
    pub(crate) fn keep_transform_target(&mut self) {
        if self.transform_tool().is_some()
            && (self.active_mesh().is_none()
                || self.current_selection().is_some()
                || self.current_inspected().is_some())
        {
            self.close_rail_tool();
        }
    }

    /// The bounding-box centre of object `id` in object space, once its
    /// bounds are known.
    fn object_centre(&self, id: ObjectId) -> Option<glam::DVec3> {
        let o = self.session.document().object(id)?;
        let bb = self.object_bounds(o)??;
        Some(glam::DVec3::from(bb.center()))
    }

    /// The active object's placement as the Transform fields show it.
    pub fn active_placement(&self) -> Option<Placement> {
        let id = self.active?;
        let t = self
            .shown_transform(id)
            .or_else(|| Some(self.session.document().object(id)?.transform))?;
        Some(placement(&t, self.object_centre(id)?))
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
        Some(gizmo(p.position, mode))
    }

    /// The handle to draw highlighted: the one dragged, else hovered.
    pub(crate) fn gizmo_highlight(&self) -> Option<Handle> {
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
    /// gizmo drag now snaps to ([`snap_step`]).
    pub(crate) fn tool_hint(&self) -> Option<String> {
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
        let (_, step_mm) = snap_step(&cam, pivot, view, self.session.document().units());
        format!(" · {}", snap_hint(step_mm))
    }

    /// The snapping a drag uses now: the shared [`Self::gizmo_snap`]
    /// switch, inverted while Cmd/Ctrl is held. The grid is a 1-2-5-mm
    /// step picked so it reads on screen at the current zoom
    /// (MF-OBJ-002, [`snap_step`] — a fixed
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
            snap_step(&cam, g.pivot, view, self.session.document().units()).0
        });
        Snap {
            grid,
            angle_deg: Some(ANGLE_SNAP_DEG),
            scale_step: Some(SCALE_SNAP_STEP),
            plate: lowest.map(|world_min_z| PlateSnap {
                world_min_z,
                within_pt: 8.0,
            }),
        }
    }

    /// Ask for the active object's lowest point at its placement (for
    /// the plate snap) while Transform is open. O(1) here; the search
    /// runs on the background pool.
    pub(crate) fn refresh_lowest(&mut self) {
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
        crate::defects::placement_change(name, doc.units(), from, to, centre)
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
        self.settling_placement = Some((object, transform));
        let mut params = self.params_for("object.set_transform").clone();
        params.insert("transform".into(), ParamValue::Affine(transform));
        let intent = crate::intent::Intent::op("object.set_transform")
            .on(object)
            .values(&params);
        self.run_intent(intent, "Transform".into());
        if !self.follow_up(FollowUp::Status(status)) {
            // It didn't start (the error banner says why).
            self.settling_placement = None;
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
        self.settling_placement = Some((object, transform));
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
        if !self.follow_up(FollowUp::Status(status.to_owned())) {
            self.settling_placement = None;
        }
        true
    }

    /// Drop to plate: the active object straight down (or up) until its
    /// lowest point rests on the plate, on the worker, with progress.
    pub fn drop_to_plate(&mut self) {
        if self.busy.is_some() || self.active.is_none() {
            return;
        }
        let Some(target) = self.active else { return };
        let params = self.params_for("object.drop_to_plate").clone();
        let intent = crate::intent::Intent::op("object.drop_to_plate")
            .on(target)
            .values(&params);
        self.run_intent(intent, "Drop to plate".into());
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
        let Some(to) = with_rotation_scale(&from, centre, glam::DVec3::ZERO, glam::DVec3::ONE)
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
                Some(with_position(&from, centre, pos))
            }
            Field::Rotation(a) => p.rotation_deg.zip(p.scale).and_then(|(mut r, s)| {
                r[a] = value;
                with_rotation_scale(&from, centre, r, s)
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
                with_rotation_scale(&from, centre, r, s)
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
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // tests may panic on failure
mod tests {
    use super::*;

    const IDENTITY: [f64; 12] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];

    #[test]
    fn rotation_about_the_centre_keeps_the_position() {
        let c = DVec3::new(5.0, 5.0, 5.0);
        let t = with_rotation_scale(&IDENTITY, c, DVec3::new(0.0, 0.0, 90.0), DVec3::ONE).unwrap();
        let p = placement(&t, c);
        assert!((p.position - c).length() < 1e-12);
        assert!((p.rotation_deg.unwrap().z - 90.0).abs() < 1e-9);
        assert_eq!(
            crate::defects::placement_change("plate", Unit::Millimeter, &IDENTITY, &t, c),
            "Rotated plate 90° about Z"
        );
    }

    #[test]
    fn status_words_for_move_and_scale() {
        let c = DVec3::ZERO;
        let moved = with_position(&IDENTITY, c, DVec3::new(12.0, 0.0, 0.0));
        assert_eq!(
            crate::defects::placement_change("scan", Unit::Millimeter, &IDENTITY, &moved, c),
            "Moved scan by 12.0, 0, 0 mm"
        );
        let scaled = with_rotation_scale(&IDENTITY, c, DVec3::ZERO, DVec3::splat(1.5)).unwrap();
        assert_eq!(
            crate::defects::placement_change("part", Unit::Millimeter, &IDENTITY, &scaled, c),
            "Scaled part by 150%"
        );
    }

    #[test]
    fn shear_has_no_rotation_or_scale_and_a_mirror_stays_mirrored() {
        let mut sheared = IDENTITY;
        sheared[3] = 0.5;
        let p = placement(&sheared, DVec3::ZERO);
        assert!(p.rotation_deg.is_none() && p.scale.is_none());
        assert!(with_rotation_scale(&sheared, DVec3::ZERO, DVec3::ZERO, DVec3::ONE).is_none());
        let mut mirrored = IDENTITY;
        mirrored[0] = -1.0;
        let p = placement(&mirrored, DVec3::ZERO);
        assert!(p.mirrored);
        assert_eq!(p.scale, Some(DVec3::ONE));
        let doubled =
            with_rotation_scale(&mirrored, DVec3::ZERO, DVec3::ZERO, DVec3::splat(2.0)).unwrap();
        assert!(placement(&doubled, DVec3::ZERO).mirrored);
    }

    #[test]
    fn field_text_is_short() {
        assert_eq!(field_text(12.0), "12");
        assert_eq!(field_text(-0.00001), "0");
        assert_eq!(field_text(1.25), "1.25");
    }
}
