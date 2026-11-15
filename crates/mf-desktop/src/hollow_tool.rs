//! The Hollow tool (MF-ISSUE-112): `mesh.hollow` on the rail's Shape
//! group, registered through `shell::Tool` like Make solid. One panel:
//! the wall thickness (a slider and a field), the detail, the drain holes
//! and a one-line trade-off; ONE primary button, "Hollow", one undo.
//!
//! While it's open the active object is previewed hollowed, on a worker
//! (the operation's own [`mf_operations::hollow::hollow_shell`], so the
//! preview is what Apply makes, less the drain holes): drawn in place of
//! the object, cut open by Split's section view so the walls show, with
//! the parts left solid because they're thinner than twice the wall
//! outlined in `THIN_WALL`. Placed drain holes are clicked on the surface.

use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};
use glam::DVec3;
use mf_command::ObjectId;
use mf_geometry::{FaceId, Mesh};
use mf_operations::hollow;
use mf_runtime::{Cancelled, RunContext};

use crate::MeshFixerApp;
use crate::intent::Intent;
use crate::shell::{Availability, Category, Input, RegisterError, Shell, Tool as T};
use crate::tools::LegendRow;
use crate::viewport::{ToolPointer, ViewLayer};
use crate::widgets::{self, Kind};
use crate::worker::{Gpu, Message, RenderMesh, UPLOAD_CHUNK_BYTES};
use crate::{icons, t, theme};

pub const HOLLOW: &str = "hollow";

/// How long settings must stay put before the preview is worked out
/// again (a slider drag asks once it stops).
const SETTLE: Duration = Duration::from_millis(250);

/// Most thin faces whose edges the overlay draws (beyond this, every
/// n-th): the region still reads, and the lines stay small.
const THIN_LINES_MAX_FACES: usize = 400_000;

/// A placed drain hole: a point on one of the object's faces.
#[derive(Debug, Clone, PartialEq)]
pub struct HolePoint {
    pub face: FaceId,
    pub bary: [f64; 3],
    /// Where it is, in the object's own space (for its marker).
    pub local: [f64; 3],
}

/// The Hollow tool's settings (document units) and its preview.
#[derive(Default)]
pub struct HollowState {
    /// Wall thickness; `None` until the object's default (2 mm) is set.
    pub wall: Option<f64>,
    /// The cell size; `None` is automatic.
    pub detail: Option<f64>,
    /// "none", "auto" or "placed".
    pub drain: String,
    pub diameter: f64,
    pub count: i64,
    /// "auto", "round", "teardrop" or "flat_top".
    pub profile: String,
    pub points: Vec<HolePoint>,
    /// Open the cavity out through `picked_region`, instead of (or as
    /// well as) a drain hole.
    pub open_through: bool,
    /// The picked surface region (stable input-face seeds), and the
    /// object's revision it was picked on (a new revision clears it).
    pub picked_region: Option<PickedRegion>,
    /// Cut the preview open (on by default).
    pub section: bool,
    pub for_object: Option<ObjectId>,
    /// When a setting last changed (the preview waits [`SETTLE`]).
    pub changed_at: Option<Instant>,
    /// The preview last asked for, and of which revision.
    asked: Option<(PreviewKey, Mesh)>,
    preview: Option<Arc<Preview>>,
    /// The revision Hollow just made: not previewed again (it would hollow
    /// the shell) until a setting changes.
    made: Option<Mesh>,
    /// The last result's sentence about thin parts.
    pub last_result: Option<String>,
}

/// A picked surface region: its faces (stable input-face seeds) on the
/// mesh it was picked from, and a name for the panel ("Cut face of scan
/// (top)").
#[derive(Debug, Clone)]
pub struct PickedRegion {
    pub faces: Vec<FaceId>,
    pub name: String,
    /// The mesh it was picked on: a new revision invalidates the pick
    /// (its face ids may no longer mean the same thing).
    mesh: Mesh,
}

/// What a preview depends on, besides the mesh (object units).
#[derive(Debug, Clone, PartialEq)]
pub struct PreviewKey {
    pub object: ObjectId,
    pub wall: f64,
    pub detail: f64,
}

pub struct PreviewRequest {
    pub key: PreviewKey,
    pub mesh: Mesh,
    /// What the view shows for the object now (its GPU chunks are reused).
    pub previous: Option<Arc<RenderMesh>>,
}

/// A worked-out preview of `mesh` (the revision asked about).
pub struct Preview {
    pub key: PreviewKey,
    pub mesh: Mesh,
    pub outcome: Result<PreviewShell, (String, String)>,
}

/// The hollowed object as the preview draws it.
pub struct PreviewShell {
    pub render: Arc<RenderMesh>,
    /// The thin parts' edges, relative to `render.origin`.
    pub thin_lines: Arc<mf_render::GpuLines>,
    pub thin_faces: usize,
    pub thin_fraction: f64,
    pub inner_faces: usize,
    /// Object units.
    pub cell: f64,
}

/// Works out a preview (on a worker): the operation's own shell, its
/// render mesh, and the thin parts' lines.
pub fn preview_work(
    req: PreviewRequest,
    ctx: &RunContext,
    gpu: &Gpu,
) -> Result<Message, Cancelled> {
    let outcome = match hollow::hollow_shell(&req.mesh, req.key.wall, req.key.detail, ctx) {
        Err(mf_operations::OpError::Cancelled) => return Err(Cancelled),
        Err(mf_operations::OpError::Failed { code, message }) => Err((code.to_owned(), message)),
        // ADR-0010, consolidation plan C14: a thin adapter rendering a
        // declared refusal the same (code, message) shape this panel
        // used before the registry existed; routing it to the panel
        // through the registry's own RefusalCard is C16's job.
        Err(mf_operations::OpError::Refused { code, args }) => Err((
            code.to_owned(),
            mf_operations::refusal::render_message(&args),
        )),
        Err(e) => Err(("invalid_parameter".to_owned(), e.to_string())),
        Ok(shell) => {
            ctx.checkpoint()?;
            let render = Arc::new(RenderMesh::build(
                gpu,
                &shell.mesh,
                &[],
                req.previous.as_deref(),
            ));
            ctx.checkpoint()?;
            // Each thin face's edges once (every n-th face past the cap).
            let step = shell.thin.len().div_ceil(THIN_LINES_MAX_FACES).max(1);
            let mut edges: Vec<[u32; 2]> = shell
                .thin
                .iter()
                .step_by(step)
                .filter_map(|&f| shell.mesh.face(f))
                .flat_map(|t| {
                    (0..3).map(move |k| [t[k].min(t[(k + 1) % 3]), t[k].max(t[(k + 1) % 3])])
                })
                .collect();
            edges.sort_unstable();
            edges.dedup();
            let vertices = mf_render::edge_line_vertices(&shell.mesh, render.origin, &edges);
            let thin_lines = Arc::new(mf_render::GpuLines::upload_paced(
                &gpu.device,
                &gpu.queue,
                &vertices,
                UPLOAD_CHUNK_BYTES,
            ));
            Ok(PreviewShell {
                render,
                thin_lines,
                thin_faces: shell.thin.len(),
                thin_fraction: if shell.outer_area > 0.0 {
                    shell.thin_area / shell.outer_area
                } else {
                    0.0
                },
                inner_faces: shell.inner.len(),
                cell: shell.cell,
            })
        }
    };
    Ok(Message::HollowPreview(Box::new(Preview {
        key: req.key,
        mesh: req.mesh,
        outcome,
    })))
}

pub fn register(shell: &mut Shell) -> Result<(), RegisterError> {
    shell.register_tool(T {
        help: "tool.hollow.help",
        availability: |app| {
            if app.active_mesh().is_some() {
                Availability::Available
            } else {
                Availability::Unavailable(crate::panels::empty_state())
            }
        },
        panel: Some(panel),
        claims,
        viewport_input: input,
        overlays: |_| Some(legend()),
        hides_defects: |_| true,
        on_open: |app| {
            app.refusal = None;
            app.hollow.made = None;
            app.hollow.last_result = None;
        },
        on_close: |app, _| {
            app.hollow.preview = None;
            app.hollow.asked = None;
            if let Some(r) = &app.hollow.picked_region
                && app.active_mesh().is_some_and(|m| Mesh::ptr_eq(m, &r.mesh))
            {
                app.selection = None;
            }
            app.hollow.picked_region = None;
            app.hollow.open_through = false;
        },
        ..T::new(
            HOLLOW,
            "tool.hollow.name",
            icons::HOLLOW,
            Category::Shape,
            25,
        )
    })
}

fn legend() -> Vec<LegendRow> {
    vec![LegendRow {
        name: "Left solid",
        color: theme::THIN_WALL,
        meaning: "Parts thinner than twice the wall: kept solid, not hollowed",
        switch: None,
    }]
}

/// The document's millimetres per unit.
fn mm_per(app: &MeshFixerApp) -> f64 {
    crate::transform_tool::mm_per(app.session.document().units())
}

/// The active object's placement.
fn transform_of(app: &MeshFixerApp, id: ObjectId) -> [f64; 12] {
    app.session
        .document()
        .object(id)
        .map(|o| o.transform)
        // allow-swallow: id is the active object, checked live by the caller; a vanished one reads as identity for this frame.
        .unwrap_or(mf_io::threemf::IDENTITY)
}

/// Settings for `id`, defaulted the first time it's shown.
fn ensure_defaults(app: &mut MeshFixerApp, id: ObjectId) {
    let mm = mm_per(app);
    let s = &mut app.hollow;
    if s.for_object != Some(id) || s.wall.is_none() {
        let section = s.for_object.is_none() || s.section;
        *s = HollowState {
            wall: Some(2.0 / mm),
            detail: None,
            drain: "none".into(),
            diameter: 3.0 / mm,
            count: 1,
            profile: "auto".into(),
            points: Vec::new(),
            open_through: false,
            picked_region: None,
            section,
            for_object: Some(id),
            changed_at: None,
            asked: None,
            preview: None,
            made: s.made.take(),
            last_result: s.last_result.take(),
        };
    }
}

fn changed(app: &mut MeshFixerApp) {
    app.hollow.changed_at = Some(Instant::now());
    app.hollow.made = None;
    app.hollow.last_result = None;
}

/// Keep the preview matching the settings while Hollow is open (called
/// every frame).
pub fn refresh_preview(app: &mut MeshFixerApp, ctx: &egui::Context) {
    if app.active_tool != HOLLOW {
        app.hollow.preview = None;
        app.hollow.asked = None;
        return;
    }
    let Some(id) = app.active else { return };
    let Some(mesh) = app.active_mesh().cloned() else {
        return;
    };
    ensure_defaults(app, id);
    if app.busy.is_some()
        || app
            .hollow
            .made
            .as_ref()
            .is_some_and(|m| Mesh::ptr_eq(m, &mesh))
    {
        return;
    }
    let scale = hollow::world_per_object(&transform_of(app, id));
    if scale.is_nan() || scale <= 0.0 {
        return;
    }
    let s = &app.hollow;
    let key = PreviewKey {
        object: id,
        wall: s.wall.unwrap_or(0.0) / scale,
        detail: s.detail.unwrap_or(0.0) / scale,
    };
    if s.asked
        .as_ref()
        .is_some_and(|(k, m)| *k == key && Mesh::ptr_eq(m, &mesh))
    {
        return;
    }
    if let Some(at) = s.changed_at
        && at.elapsed() < SETTLE
    {
        ctx.request_repaint_after(SETTLE - at.elapsed());
        return;
    }
    let previous = app.active_render();
    app.hollow.asked = Some((key.clone(), mesh.clone()));
    app.hollow_worker.submit(PreviewRequest {
        key,
        mesh,
        previous,
    });
}

/// A preview came back: kept if it's still the one asked for.
pub fn on_preview(app: &mut MeshFixerApp, p: Preview) {
    if app
        .hollow
        .asked
        .as_ref()
        .is_some_and(|(k, m)| *k == p.key && Mesh::ptr_eq(m, &p.mesh))
    {
        app.hollow.preview = Some(Arc::new(p));
    }
}

/// The preview for `id`'s current revision, if it's worked out.
fn current_preview(app: &MeshFixerApp, id: ObjectId) -> Option<&Preview> {
    if app.active_tool != HOLLOW || app.active != Some(id) {
        return None;
    }
    let p = app.hollow.preview.as_deref()?;
    let (key, mesh) = app.hollow.asked.as_ref()?;
    (key.object == id
        && *key == p.key
        && Mesh::ptr_eq(mesh, &p.mesh)
        && app.active_mesh().is_some_and(|m| Mesh::ptr_eq(m, &p.mesh)))
    .then_some(p)
}

/// The hollowed preview's render mesh, drawn in place of `id`'s own while
/// Hollow is open.
pub fn preview_render(app: &MeshFixerApp, id: ObjectId) -> Option<Arc<RenderMesh>> {
    let p = current_preview(app, id)?;
    // allow-swallow: a refused preview draws the object as it is; its refusal shows in the panel
    p.outcome.as_ref().ok().map(|s| s.render.clone())
}

/// The thin parts' outline layer, over the preview.
pub fn thin_layer(app: &MeshFixerApp) -> Option<ViewLayer> {
    let id = app.active?;
    // allow-swallow: a refused preview has no thin parts to draw; its refusal shows in the panel
    let s = current_preview(app, id)?.outcome.as_ref().ok()?;
    (s.thin_faces > 0).then(|| ViewLayer {
        object: id,
        origin: s.render.origin,
        lines: s.thin_lines.clone(),
        color: theme::THIN_WALL,
        xray: false,
        emphasis: 0.0,
    })
}

/// The section view's plane (point, normal; world) while the preview is
/// shown: upright through the object's centre, facing the camera, so the
/// near half is ghosted and the walls show where it's cut.
pub fn section_plane(app: &MeshFixerApp) -> Option<(DVec3, DVec3)> {
    let id = app.active?;
    if !app.hollow.section || preview_render(app, id).is_none() {
        return None;
    }
    let (lo, hi) = crate::shape_tools::world_bounds(app, id)?;
    let centre = (lo + hi) * 0.5;
    let toward = app
        .camera
        .map_or(DVec3::Y, |c| c.eye() - centre)
        .with_z(0.0)
        .try_normalize()
        .unwrap_or(DVec3::Y);
    Some((centre, toward))
}

/// While Hollow is open, a press on the active object's surface is the
/// tool's (placing a drain hole when Placed is chosen, else picking the
/// surface region `open_through` opens through); elsewhere the camera
/// and picking keep it.
fn claims(app: &MeshFixerApp) -> Option<crate::shell::Claims> {
    if app.busy.is_some() {
        return None;
    }
    let id = app.active?;
    let (cam, rect) = (app.camera?, app.viewport.last_rect?);
    let size = (rect.width() as f64, rect.height() as f64);
    let bvh = app.picking_bvh(id)?;
    let mesh = app.active_mesh()?.clone();
    let model = mf_render::model_matrix(&transform_of(app, id));
    Some(Box::new(move |px: (f64, f64)| {
        let (origin, dir) = cam.ray(cam.target, size, px);
        bvh.ray_pick(&mesh, origin, dir, &model).is_some()
    }))
}

/// A click in the view while Hollow is open: a drain hole where it
/// meets the object (Placed), or the surface region under it, for
/// `open_through` (any other drain mode).
fn input(app: &mut MeshFixerApp, input: &crate::shell::ToolInput) -> Input {
    let Some(ToolPointer::Click(px)) = input.pointer else {
        return Input::Pass;
    };
    if app.busy.is_some() {
        return Input::Pass;
    }
    let (Some(id), Some(cam), Some(rect)) = (app.active, app.camera, app.viewport.last_rect) else {
        return Input::Pass;
    };
    let view = (rect.width() as f64, rect.height() as f64);
    let (origin, dir) = cam.ray(cam.target, view, px);
    let Some(bvh) = app.picking_bvh(id) else {
        return Input::Pass;
    };
    let Some(mesh) = app.active_mesh().cloned() else {
        return Input::Pass;
    };
    let transform = transform_of(app, id);
    let model = mf_render::model_matrix(&transform);
    let Some((face, t)) = bvh.ray_pick(&mesh, origin, dir, &model) else {
        return Input::Pass;
    };
    if app.hollow.drain == "placed" {
        let world = DVec3::from(origin) + DVec3::from(dir) * t;
        let local = model.inverse().transform_point3(world);
        let Some(tri) = mesh.face(face) else {
            return Input::Pass;
        };
        let [a, b, c] = tri.map(|v| DVec3::from(mesh.corner_position(v)));
        let bary = barycentric(local, a, b, c);
        let local = a * bary[0] + b * bary[1] + c * bary[2];
        app.hollow.points.push(HolePoint {
            face,
            bary,
            local: local.to_array(),
        });
        changed(app);
        return Input::Consumed;
    }
    // A recorded split cap, picked exactly as the cut itself defines it
    // (its own plane and side), never grown from the seed by a
    // geometric heuristic: crease growth is the fallback only for a
    // surface that isn't one (owner 2026-11-13 follow-up).
    let (region, name) = match recorded_cap_region(app, id, &mesh, &bvh, &transform, face) {
        Some((faces, name)) => (faces, name),
        None => (grow_region(&mesh, face), region_name(app, id, &mesh, face)),
    };
    let ids: Vec<u32> = region.iter().map(|f| f.0).collect();
    app.hollow.picked_region = Some(PickedRegion {
        faces: region,
        name,
        mesh,
    });
    // The selection tint and outline (the generic mechanism every other
    // face-region highlight here uses): a plain click elsewhere in the
    // view never reaches this (Hollow's own `claims` keeps it), so this
    // never fights the row/click selection while the tool is open.
    app.selection = Some(mf_selection::Selection {
        object: id,
        revision: app.session.current(),
        kind: mf_selection::SelectionKind::Faces,
        ids,
    });
    Input::Consumed
}

/// `face` as a recorded split cap, if it is one: a cut (`split_flow`'s
/// own joints) remembers its plane and which piece is which side of
/// it, so that piece's cap is found exactly — `mf_operations::cap_faces`
/// against the joint's own recorded frame, never grown from a seed —
/// whenever `id` is one of the joint's two pieces and `face` is on
/// that side's cap. `None` for a surface that isn't a recorded cap (or
/// no longer matches one, the cut having been undone or redone past);
/// the caller falls back to crease growth.
fn recorded_cap_region(
    app: &MeshFixerApp,
    id: ObjectId,
    mesh: &Mesh,
    bvh: &mf_geometry::Bvh,
    transform: &[f64; 12],
    face: FaceId,
) -> Option<(Vec<FaceId>, String)> {
    for joint in &app.split_flow.joints {
        let side = if joint.a == id {
            mf_operations::JointSide::A
        } else if joint.b == id {
            mf_operations::JointSide::B
        } else {
            continue;
        };
        let Ok(faces) = mf_operations::cap_faces(
            mesh,
            Some(bvh),
            transform,
            &joint.frame,
            side,
            &RunContext::none(),
        ) else {
            continue;
        };
        if !faces.iter().any(|&(f, _)| f == face) {
            continue;
        }
        let ids: Vec<FaceId> = faces.iter().map(|&(f, _)| f).collect();
        let base = app
            .session
            .document()
            .object(id)
            .map(|o| o.name.clone())
            // allow-swallow: id is the active object, checked live by the caller; a vanished one reads as an empty name for this frame.
            .unwrap_or_default();
        let (side_a, side_b) = mf_operations::split_names(
            &format!("Cut face of {base}"),
            joint.frame.normal.to_array(),
        );
        let name = match side {
            mf_operations::JointSide::A => side_a,
            mf_operations::JointSide::B => side_b,
        };
        return Some((ids, name));
    }
    // A single-piece keep (Above/Below/Top/Bottom) records no joint
    // (one needs both pieces), but the cut's own cap is still known
    // exactly: every object's own cap from the most recent cut that
    // made one (owner 2026-11-14 follow-up).
    for &(cap_id, ref frame) in &app.split_flow.cut_caps {
        if cap_id != id {
            continue;
        }
        // Which side of the plane this object's own kept material is
        // on isn't recorded here (only the plane itself is, the same
        // for either `keep`): tried both, whichever one's own cap
        // contains the clicked face is this object's.
        for side in [mf_operations::JointSide::A, mf_operations::JointSide::B] {
            let Ok(faces) = mf_operations::cap_faces(
                mesh,
                Some(bvh),
                transform,
                frame,
                side,
                &RunContext::none(),
            ) else {
                continue;
            };
            if !faces.iter().any(|&(f, _)| f == face) {
                continue;
            }
            let ids: Vec<FaceId> = faces.iter().map(|&(f, _)| f).collect();
            let base = app
                .session
                .document()
                .object(id)
                .map(|o| o.name.clone())
                // allow-swallow: id is the active object, checked live by the caller; a vanished one reads as an empty name for this frame.
                .unwrap_or_default();
            let outward = match side {
                mf_operations::JointSide::A => frame.normal.to_array(),
                mf_operations::JointSide::B => (-frame.normal).to_array(),
            };
            // `split_names`'s own first element is always "the side the
            // normal points to" — exactly `outward` here.
            let (name, _) = mf_operations::split_names(&format!("Cut face of {base}"), outward);
            return Some((ids, name));
        }
    }
    None
}

/// A name for the picked region, for the panel ("Cut face of scan
/// (top)" for a planar cap aligned with an axis; "Picked surface of
/// scan" otherwise).
fn region_name(app: &MeshFixerApp, id: ObjectId, mesh: &Mesh, seed: FaceId) -> String {
    let base = app
        .session
        .document()
        .object(id)
        .map(|o| o.name.clone())
        // allow-swallow: id is the active object, checked live by the caller; a vanished one reads as an empty name for this frame.
        .unwrap_or_default();
    let Some(t) = mesh.face(seed) else {
        return format!("Picked surface of {base}");
    };
    let p = t.map(|v| mesh.corner_position(v));
    let (a, b, c) = (
        glam::DVec3::from(p[0]),
        glam::DVec3::from(p[1]),
        glam::DVec3::from(p[2]),
    );
    let Some(normal) = (b - a).cross(c - a).try_normalize() else {
        return format!("Picked surface of {base}");
    };
    // The cap's own outward normal is the direction the names are built
    // from, so it's always "side a"'s name (the side a normal points to).
    let (side_a, _) = mf_operations::split_names(&format!("Cut face of {base}"), normal.to_array());
    side_a
}

/// `p`'s barycentric coordinates on triangle `abc` (projected onto its
/// plane, clamped inside it).
fn barycentric(p: DVec3, a: DVec3, b: DVec3, c: DVec3) -> [f64; 3] {
    let (v0, v1, v2) = (b - a, c - a, p - a);
    let (d00, d01, d11) = (v0.dot(v0), v0.dot(v1), v1.dot(v1));
    let (d20, d21) = (v2.dot(v0), v2.dot(v1));
    let den = d00 * d11 - d01 * d01;
    if den.abs() < 1e-300 {
        return [1.0 / 3.0; 3];
    }
    let v = ((d11 * d20 - d01 * d21) / den).clamp(0.0, 1.0);
    let w = ((d00 * d21 - d01 * d20) / den).clamp(0.0, 1.0 - v);
    [1.0 - v - w, v, w]
}

/// Past this many degrees between two adjacent faces' normals, the
/// region stops growing (a sharp crease): for a planar split cap every
/// face is coplanar (0 degrees) so the whole cap is picked up to its
/// boundary; a curved (organic) surface keeps growing across gentle
/// curvature but stops at a real edge.
pub const CREASE_DEGREES: f64 = 60.0;

/// The whole region a click at `seed` belongs to: every face reachable
/// from it through edges whose two faces' normals are within
/// [`CREASE_DEGREES`] of each other — a planar cap's whole disk up to
/// its boundary (every face there is coplanar), or a curved surface's
/// patch up to its first sharp crease. `mesh`'s own face ids are used
/// directly (stable input-face seeds for replay).
pub fn grow_region(mesh: &Mesh, seed: FaceId) -> Vec<FaceId> {
    let cos_limit = CREASE_DEGREES.to_radians().cos();
    let normal = |f: FaceId| -> Option<[f64; 3]> {
        let t = mesh.face(f)?;
        let p = t.map(|v| mesh.corner_position(v));
        let (a, b, c) = (
            glam::DVec3::from(p[0]),
            glam::DVec3::from(p[1]),
            glam::DVec3::from(p[2]),
        );
        (b - a).cross(c - a).try_normalize().map(|n| n.to_array())
    };
    // Undirected edge (low, high vertex) -> the faces sharing it (at
    // most 2 on a manifold surface; more are never grown across).
    let mut by_edge: std::collections::HashMap<(u32, u32), Vec<FaceId>> =
        std::collections::HashMap::new();
    for (f, t) in mesh.faces() {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            let key = (a.min(b), a.max(b));
            by_edge.entry(key).or_default().push(f);
        }
    }
    let Some(seed_n) = normal(seed) else {
        return vec![seed];
    };
    let mut region = vec![seed];
    let mut normals: std::collections::HashMap<FaceId, [f64; 3]> =
        std::collections::HashMap::from([(seed, seed_n)]);
    let mut frontier = vec![seed];
    while let Some(f) = frontier.pop() {
        let Some(t) = mesh.face(f) else { continue };
        let Some(&fn_) = normals.get(&f) else {
            continue;
        };
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            let key = (a.min(b), a.max(b));
            let Some(neighbours) = by_edge.get(&key) else {
                continue;
            };
            if neighbours.len() != 2 {
                continue; // a boundary or non-manifold edge: the region's own edge
            }
            for &g in neighbours {
                if g == f || normals.contains_key(&g) {
                    continue;
                }
                let Some(gn) = normal(g) else { continue };
                let cos = glam::DVec3::from(fn_).dot(glam::DVec3::from(gn));
                if cos >= cos_limit {
                    normals.insert(g, gn);
                    region.push(g);
                    frontier.push(g);
                }
            }
        }
    }
    region.sort_unstable_by_key(|f| f.0);
    region
}

/// The placed holes' markers over the view.
pub fn overlay(app: &MeshFixerApp, painter: &egui::Painter, rect: egui::Rect) {
    if app.active_tool != HOLLOW || app.hollow.drain != "placed" {
        return;
    }
    let (Some(id), Some(cam)) = (app.active, app.camera) else {
        return;
    };
    let transform = transform_of(app, id);
    let view = (rect.width() as f64, rect.height() as f64);
    for p in &app.hollow.points {
        let w = crate::transform_tool::world_point(&transform, DVec3::from(p.local));
        if let Some((x, y)) = mf_render::screen_position(&cam, cam.target, view, w.to_array()) {
            let at = rect.left_top() + egui::vec2(x as f32, y as f32);
            painter.circle_stroke(at, 6.0, egui::Stroke::new(2.0, theme::TEXT_STRONG));
            painter.circle_filled(at, 2.0, theme::TEXT_STRONG);
        }
    }
}

/// After a command: a Hollow's result is the new revision, not previewed
/// again until a setting changes; its sentence about thin parts is kept.
pub fn after_commit(app: &mut MeshFixerApp, op: &str, report: &serde_json::Value) {
    if op != "mesh.hollow" {
        return;
    }
    app.hollow.made = app.active_mesh().cloned();
    app.hollow.preview = None;
    app.hollow.asked = None;
    app.hollow.points.clear();
    app.hollow.picked_region = None;
    app.hollow.open_through = false;
    app.selection = None;
    app.hollow.last_result = Some(match report["thin"]["below"].as_f64() {
        // allow-swallow: a report without a thin-face count has none to report
        Some(below) if report["thin"]["faces"].as_u64().unwrap_or(0) > 0 => t!(
            "hollow.thin",
            size = hollow::short(below),
            unit = crate::defects::unit_symbol(app.session.document().units())
        ),
        _ => t!("hollow.done"),
    });
}

fn section(ui: &mut egui::Ui, text: &str) {
    ui.add_space(theme::SPACE_M);
    ui.label(
        RichText::new(text)
            .size(theme::TYPE_SECTION)
            .color(theme::TEXT_STRONG),
    );
    ui.add_space(2.0);
}

fn panel(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    crate::shape_tools::refusal_card_for(app, ui, "mesh.hollow");
    let units = crate::defects::unit_symbol(app.session.document().units());
    let Some(id) = app.active else { return };
    ensure_defaults(app, id);
    // A pick from a revision that's no longer shown (undo, redo, an
    // edit elsewhere) doesn't mean what it used to.
    if let Some(r) = &app.hollow.picked_region
        && !app.active_mesh().is_some_and(|m| Mesh::ptr_eq(m, &r.mesh))
    {
        app.hollow.picked_region = None;
        app.hollow.open_through = false;
        app.selection = None;
    }
    let Some((lo, hi)) = crate::shape_tools::world_bounds(app, id) else {
        widgets::caption(ui, t!("hollow.measuring"));
        return;
    };
    let diag = (hi - lo).length().max(1e-9);
    widgets::body(ui, t!("hollow.what"));

    // Wall thickness: a slider and a field.
    section(ui, &t!("hollow.section.wall", unit = units));
    let mm = mm_per(app);
    let mut wall = app.hollow.wall.unwrap_or(2.0 / mm);
    let before = wall;
    ui.horizontal(|ui| {
        ui.spacing_mut().slider_width = (ui.available_width() - 90.0).max(60.0);
        let r = ui.add(
            egui::Slider::new(&mut wall, (0.4 / mm)..=(10.0 / mm))
                .logarithmic(true)
                .show_value(false),
        );
        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Slider, true, "Wall thickness"));
        ui.add(
            egui::DragValue::new(&mut wall)
                .speed(0.02)
                .range((0.01 / mm)..=(diag / 2.0))
                .suffix(format!(" {units}"))
                .max_decimals(2),
        )
        .on_hover_text(t!("hollow.wall_help"));
    });
    if wall != before {
        app.hollow.wall = Some(wall);
        changed(app);
    }
    widgets::caption(ui, t!("hollow.tradeoff"));

    // Detail.
    section(ui, &t!("hollow.section.detail", unit = units));
    let scale = hollow::world_per_object(&transform_of(app, id));
    let auto = hollow::cell_size(0.0, wall, diag);
    let mut automatic = app.hollow.detail.is_none();
    if ui
        .checkbox(&mut automatic, t!("hollow.detail_auto"))
        .on_hover_text(t!("hollow.detail_help"))
        .changed()
    {
        app.hollow.detail = if automatic { None } else { Some(auto) };
        changed(app);
    }
    match app.hollow.detail {
        None => {
            widgets::caption(
                ui,
                t!(
                    "hollow.detail_is",
                    cell = crate::defects::measure(auto),
                    unit = units
                ),
            );
        }
        Some(mut cell) => {
            let before = cell;
            ui.scope(|ui| {
                ui.spacing_mut().slider_width = (ui.available_width() - 90.0).max(60.0);
                let r = ui.add(
                    egui::Slider::new(&mut cell, (wall / 16.0)..=(wall / 2.0))
                        .logarithmic(true)
                        .suffix(format!(" {units}"))
                        .max_decimals(3),
                );
                r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Slider, true, "Detail"));
            });
            if cell != before {
                app.hollow.detail = Some(cell);
                changed(app);
            }
        }
    }

    // Open through the picked surface.
    section(ui, &t!("hollow.section.open_through"));
    let picked = app.hollow.picked_region.clone();
    ui.add_enabled_ui(picked.is_some(), |ui| {
        ui.checkbox(&mut app.hollow.open_through, t!("hollow.open_through"))
            .on_hover_text(t!("hollow.open_through_help"));
    });
    if app.hollow.open_through {
        app.hollow.open_through = app.hollow.open_through && picked.is_some();
    }
    match &picked {
        Some(r) => widgets::caption(ui, t!("hollow.open_through_picked", name = r.name.clone())),
        None => widgets::caption(ui, t!("hollow.open_through_needs_pick")),
    };

    // Drain holes.
    section(ui, &t!("hollow.section.holes"));
    widgets::caption(ui, t!("hollow.holes_help"));
    let modes = [
        ("none", t!("hollow.holes.none")),
        ("auto", t!("hollow.holes.auto")),
        ("placed", t!("hollow.holes.placed")),
    ];
    let options: Vec<(&str, &str, bool)> = modes
        .iter()
        .map(|(_, l)| (l.as_str(), l.as_str(), true))
        .collect();
    let current = modes
        .iter()
        .position(|(k, _)| *k == app.hollow.drain)
        // allow-swallow: drain is always one of the listed modes (set only from this list)
        .unwrap_or(0);
    if let (Some(i), _) = widgets::segmented(ui, &options, current) {
        app.hollow.drain = modes[i].0.into();
    }
    if app.hollow.drain == "none" {
        widgets::caption(ui, t!("hollow.holes_hint"));
    }
    if app.hollow.drain != "none" {
        ui.add_space(theme::SPACE_XS);
        ui.horizontal(|ui| {
            ui.label(t!("hollow.holes.diameter", unit = units));
            ui.add(
                egui::DragValue::new(&mut app.hollow.diameter)
                    .speed(0.05)
                    .range((0.1 / mm)..=(diag / 4.0))
                    .suffix(format!(" {units}"))
                    .max_decimals(2),
            );
        });
        if app.hollow.drain == "auto" {
            widgets::caption(ui, t!("hollow.holes.auto_help"));
            ui.horizontal(|ui| {
                ui.label(t!("hollow.holes.count"));
                ui.add(egui::DragValue::new(&mut app.hollow.count).range(1..=16));
            });
        } else {
            widgets::caption(ui, t!("hollow.holes.placed_help"));
            let n = app.hollow.points.len();
            ui.horizontal(|ui| {
                ui.label(t!("hollow.holes.placed_count", n = n));
                if n > 0
                    && ui
                        .add(egui::Button::new(t!("hollow.holes.clear")).small())
                        .clicked()
                {
                    app.hollow.points.clear();
                }
            });
        }
        let profiles = [
            ("auto", t!("hole.auto")),
            ("round", t!("hole.round")),
            ("teardrop", t!("hole.teardrop")),
            ("flat_top", t!("hole.flat_top")),
        ];
        ui.horizontal(|ui| {
            ui.label(t!("hollow.holes.profile"));
            let shown = profiles
                .iter()
                .find(|(k, _)| *k == app.hollow.profile)
                .map_or_else(String::new, |(_, l)| l.clone());
            egui::ComboBox::from_id_salt("hollow hole profile")
                .selected_text(shown)
                .show_ui(ui, |ui| {
                    for (k, l) in &profiles {
                        ui.selectable_value(&mut app.hollow.profile, (*k).to_owned(), l);
                    }
                });
        });
    }

    // The preview.
    section(ui, &t!("hollow.section.preview"));
    ui.checkbox(&mut app.hollow.section, t!("hollow.section_view"))
        .on_hover_text(t!("hollow.section_view.tip"));
    let preview = current_preview(app, id);
    if let Some(text) = &app.hollow.last_result {
        widgets::chip(ui, icons::OK, theme::TEXT, text);
    } else {
        match preview.map(|p| &p.outcome) {
            None => {
                widgets::caption(ui, t!("hollow.previewing"));
            }
            Some(Ok(s)) => {
                widgets::caption(
                    ui,
                    t!(
                        "hollow.preview_ready",
                        faces = s.inner_faces,
                        cell = crate::defects::measure(s.cell * scale),
                        unit = units
                    ),
                );
                if s.thin_faces > 0 {
                    widgets::chip(
                        ui,
                        icons::WARNING,
                        theme::THIN_WALL,
                        &t!(
                            "hollow.thin",
                            size = hollow::short(2.0 * wall),
                            unit = units
                        ),
                    );
                    widgets::caption(
                        ui,
                        t!(
                            "hollow.thin_share",
                            percent = format!("{:.0}", s.thin_fraction * 100.0)
                        ),
                    );
                }
            }
            Some(Err((code, message))) => {
                widgets::card(ui, |ui| {
                    ui.label(RichText::new(t!("hollow.preview_refused")).color(theme::TEXT_STRONG));
                    widgets::body(ui, message);
                    widgets::caption(ui, format!("({code})"));
                });
            }
        }
    }

    ui.add_space(theme::SPACE_L);
    let ready = app.busy.is_none()
        && (app.hollow.drain != "placed" || !app.hollow.points.is_empty())
        && (!app.hollow.open_through || app.hollow.picked_region.is_some());
    if ui
        .add_enabled_ui(ready, |ui| {
            widgets::wide_button(ui, Kind::Primary, Some(icons::HOLLOW), &t!("hollow.apply"))
        })
        .inner
        .clicked()
    {
        let s = &app.hollow;
        let points: Vec<String> = s
            .points
            .iter()
            .map(|p| format!("{},{},{},{},0", p.face.0, p.bary[0], p.bary[1], p.bary[2]))
            .collect();
        // `wall`, `s.detail` and `s.diameter` are document units (the
        // panel's own sliders and fields), handed over as lengths in that
        // unit: `Session::prepare` alone converts them.
        let doc = app.session.document().units();
        let len = |v: f64| mf_units::Length::new(v, doc);
        let mut intent = Intent::op("mesh.hollow")
            .on(id)
            .set("wall_thickness", len(wall))
            .set("inner_detail", len(s.detail.unwrap_or(0.0)))
            .param("drain_holes", &s.drain)
            .set("hole_diameter", len(s.diameter))
            .param("hole_count", s.count)
            .param("hole_profile", &s.profile)
            .param("open_through", s.open_through);
        if s.drain == "placed" {
            intent = intent.param("hole_points", points.join(";"));
        }
        if s.open_through
            && let Some(r) = &s.picked_region
        {
            let faces = r
                .faces
                .iter()
                .map(|f| f.0.to_string())
                .collect::<Vec<_>>()
                .join(",");
            intent = intent.param("open_through_faces", faces);
        }
        app.refusal = None;
        app.run_intent(intent, t!("hollow.apply"));
    }
}

#[cfg(test)]
mod grow_region_tests {
    #![allow(clippy::unwrap_used)] // tests may panic on failure
    use super::*;

    /// Clicking a cube's flat top grows exactly its two triangles, never
    /// crossing the sharp (90 degree) crease to a side face: a flat
    /// region's own boundary is a real crease, not a naive per-edge-hop
    /// count that a smoothly curved surface could ever add up past 60
    /// degrees (unlike a curved surface's many small hops, which never
    /// trips a single-hop threshold — see
    /// `growing_across_a_smooth_surface_never_stops_on_curvature_alone`
    /// below for why a curved cap needs its own real crease to stop at,
    /// not just "far enough").
    #[test]
    fn growing_from_a_flat_top_stops_at_its_own_sharp_edge() {
        let cube = mf_fixtures::cube(10.0);
        // Faces 0, 1 are the top (+z): see `mf_fixtures::cube`.
        let region = grow_region(&cube, FaceId(0));
        let mut ids: Vec<u32> = region.iter().map(|f| f.0).collect();
        ids.sort_unstable();
        assert_eq!(ids, [0, 1], "the whole top, and no side face");
    }

    /// A UV sphere has no sharp crease anywhere: growing from any face
    /// reaches every face of it (the whole closed surface), which is
    /// the documented, accepted limit of a per-edge dihedral check on a
    /// smoothly curved surface with no real crease to stop at.
    #[test]
    fn growing_across_a_smooth_surface_never_stops_on_curvature_alone() {
        let sphere = mf_fixtures::uv_sphere(16, 24);
        let region = grow_region(&sphere, FaceId(0));
        assert_eq!(region.len(), sphere.face_count(), "the whole sphere");
    }
}
