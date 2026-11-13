//! The Sculpt tool on the rail (Phase 1 Task 19), registered through
//! `shell::Tool` like Split, Combine and Make solid (`shape_tools.rs`).
//!
//! A stroke (press to release) runs `mf_operations::sculpt::StrokeState`
//! synchronously on the UI thread: one dab is documented as local work
//! (milliseconds), so a stroke feels live with no background worker. Each
//! dab's result is turned into fresh `MeshVertices` built against the
//! stroke's own running copy (`MeshVertices::build_from`), then synced to
//! the GPU in place (`GpuMesh::sync`): only the chunks the dab touched are
//! re-uploaded (MF-RENDER-003, MF-SCULPT-004), after a one-time full
//! upload when the stroke starts. The active object's picking BVH is left
//! alone until the stroke commits; the engine's own `drift` margin keeps
//! `SculptIndex` queries correct against a moving surface meanwhile, so
//! there's no need to refit it dab by dab.
//!
//! Releasing the pointer replays the recorded samples through
//! `mesh.sculpt_stroke` (one undo step, through the usual job pool, like
//! any other operation); Esc drops the stroke with no command run.

use std::collections::BTreeMap;

use eframe::egui::{self, RichText};
use glam::DVec3;
use mf_command::ObjectId;
use mf_geometry::Mesh;
use mf_operations::sculpt::{BrushKind, BrushParams, DabSample, Falloff, SculptIndex, StrokeState};
use mf_render::MeshVertices;
use mf_spec::{ParamValue, Params};

use crate::MeshFixerApp;
use crate::shell::{Availability, Category, Claims, Close, Input, RegisterError, Shell, Tool as T};
use crate::viewport::ToolPointer;
use crate::widgets;
use crate::{icons, t, theme};

pub const SCULPT: &str = "sculpt";

/// Every brush, in the order the panel lists them.
const BRUSHES: [BrushKind; 8] = BrushKind::ALL;
/// Every falloff, in the order the panel lists them.
const FALLOFFS: [Falloff; 4] = [
    Falloff::Smooth,
    Falloff::Sphere,
    Falloff::Sharp,
    Falloff::Flat,
];
/// Dab spacing, as a fraction of the brush radius (brief: 25%).
const DAB_SPACING: f64 = 0.25;

/// The brush panel's settings. Persists while the tool is open and across
/// strokes.
#[derive(Debug, Clone)]
pub struct SculptSettings {
    pub brush: BrushKind,
    /// World-space (the document's own units, like Split's gap: M4
    /// `shape_tools.rs` house style passes GUI values straight through as
    /// the spec's `unit: mm` parameters).
    pub radius: f64,
    pub strength: f64,
    pub falloff: Falloff,
    pub symmetry: [bool; 3],
    pub detail_on: bool,
    pub detail: f64,
    /// "Stay on this surface" (on by default): the brush never spills
    /// across a sharp crease or into another recorded face group (a
    /// split cap's), and its falloff follows the surface.
    pub stay_on_surface: bool,
}

impl Default for SculptSettings {
    fn default() -> SculptSettings {
        SculptSettings {
            brush: BrushKind::Inflate,
            radius: 5.0,
            strength: 0.5,
            falloff: Falloff::Smooth,
            symmetry: [false, false, false],
            detail_on: false,
            detail: 0.25,
            stay_on_surface: true,
        }
    }
}

/// A stroke in progress (one undo step when it commits).
pub struct SculptStroke {
    pub object: ObjectId,
    state: StrokeState,
    samples: Vec<DabSample>,
    /// The vertices `render.gpu` was last synced to (this stroke's own
    /// running copy: not the object's displayed `RenderMesh`, which keeps
    /// none).
    vertices: MeshVertices,
    /// What the view draws for the object while the stroke is open
    /// (`object_views` substitutes it for the active object's usual
    /// render mesh): the only `Arc` to it, so each dab syncs `gpu` in
    /// place through `Arc::get_mut` rather than rebuilding the whole
    /// thing (`GpuMesh` holds GPU buffers directly, so it isn't `Clone`).
    pub render: std::sync::Arc<crate::worker::RenderMesh>,
    /// Where the pointer last dabbed, in world space: the next dab only
    /// samples once the pointer has moved [`DAB_SPACING`] of the radius.
    last_dab: Option<DVec3>,
    /// The brush's world-space radius (dab spacing is a fraction of it).
    radius_world: f64,
}

impl SculptStroke {
    /// Dabs recorded so far (tests: how far a drag has sampled).
    pub fn dab_count(&self) -> usize {
        self.samples.len()
    }
}

fn needs_object(app: &MeshFixerApp) -> Availability {
    if app.active_mesh().is_some() {
        Availability::Available
    } else {
        Availability::Unavailable(crate::panels::empty_state())
    }
}

pub fn register(shell: &mut Shell) -> Result<(), RegisterError> {
    shell.register_tool(T {
        help: "tool.sculpt.help",
        availability: needs_object,
        panel: Some(panel),
        claims,
        viewport_input,
        hides_defects: |app| app.sculpt_stroke.is_some(),
        keys: sculpt_keys,
        on_key: sculpt_on_key,
        on_open,
        on_close: |app, close| {
            end_stroke(app, close);
            app.sculpt_baseline = None;
        },
        ..T::new(
            SCULPT,
            "tool.sculpt.name",
            icons::SCULPT,
            Category::Shape,
            30,
        )
    })
}

/// "Show sculpt changes" (off by default) measures from the active
/// object's mesh as of right now: snapshot it when the tool opens.
fn on_open(app: &mut MeshFixerApp) {
    app.sculpt_baseline = app.active.zip(app.active_mesh().cloned());
}

/// Sculpt claims a press wherever it lands on the active object's
/// *visible* surface (a stroke, not the camera's orbit), while a stroke
/// isn't already open mid-drag (never pulled from under it): with one
/// open, `viewport_input`'s own `Move`/`End` handling continues it
/// regardless of where the pointer strays, same as any other drag. Front
/// faces only, same rule and reasoning as [`surface_point`]: claiming a
/// press that `surface_point` would then refuse (no dab) would start a
/// drag that visibly does nothing.
fn claims(app: &MeshFixerApp) -> Option<Claims> {
    if app.sculpt_stroke.is_some() {
        return Some(Box::new(|_| true) as Claims);
    }
    let id = app.active?;
    let bvh = app.picking_bvh(id)?;
    let mesh = app.active_mesh()?.clone();
    let cam = app.camera?;
    let rect = app.viewport.last_rect?;
    let transform = app.shown_transform(id).unwrap_or_else(|| {
        app.session
            .document()
            .object(id)
            .map(|o| o.transform)
            // allow-swallow: id is checked live just above (picking_bvh/active_mesh); a vanished object falls back to identity rather than panicking this frame.
            .unwrap_or_default()
    });
    let size = (rect.width() as f64, rect.height() as f64);
    Some(Box::new(move |px: (f64, f64)| {
        let (origin, dir) = cam.ray(cam.target, size, px);
        let model = mf_render::model_matrix(&transform);
        let Some((face, _)) = bvh.ray_pick(&mesh, origin, dir, &model) else {
            return false;
        };
        let Some(corners) = mesh.face(face) else {
            return false;
        };
        let [a, b, c] = corners.map(|v| DVec3::from(mesh.corner_position(v)));
        let [wa, wb, wc] = [a, b, c].map(|p| model.transform_point3(p));
        let world_normal = (wb - wa).cross(wc - wa);
        world_normal.dot(DVec3::from(dir)) < 0.0
    }) as Claims)
}

// ---------------------------------------------------------------------
// Dab targeting: a pointer position becomes a point on a face.
// ---------------------------------------------------------------------

/// The barycentric weights of `p` (on the triangle's plane) in triangle
/// `a, b, c`.
fn barycentric(p: DVec3, a: DVec3, b: DVec3, c: DVec3) -> [f64; 3] {
    let (v0, v1, v2) = (b - a, c - a, p - a);
    let d00 = v0.dot(v0);
    let d01 = v0.dot(v1);
    let d11 = v1.dot(v1);
    let d20 = v2.dot(v0);
    let d21 = v2.dot(v1);
    let denom = d00 * d11 - d01 * d01;
    if denom.abs() < 1e-300 {
        return [1.0, 0.0, 0.0];
    }
    let v = (d11 * d20 - d01 * d21) / denom;
    let w = (d00 * d21 - d01 * d20) / denom;
    [1.0 - v - w, v, w]
}

/// Where a screen point lands on object `id`'s current surface: a dab
/// sample on `mesh` (a point on a face, barycentric) and the hit point in
/// world space. `None` off the surface, or before the object has a
/// picking BVH to search against (Sculpt reuses it rather than building
/// its own: see the module docs).
///
/// `Bvh::ray_pick` finds the nearest crossing of either winding (the
/// general picking contract every tool shares — `occluded`'s own tests
/// need that). For Sculpt specifically, the stroke's own invariant is
/// narrower: a dab only ever comes from the *visible* surface under the
/// cursor, so a hit whose face points away from the camera (back-facing)
/// is refused here, even though the BVH itself doesn't distinguish it
/// from a front-facing one. On an ordinary closed, outward-facing mesh
/// the nearest hit is always front-facing anyway (the surface blocks the
/// ray before it reaches anything behind it); this only matters once the
/// mesh is open, self-intersecting, or has an inverted face already on
/// it — exactly the state a bad drag could otherwise leave it in, which
/// must never become a place a *new* stroke can start from.
fn surface_point(
    app: &MeshFixerApp,
    id: ObjectId,
    mesh: &Mesh,
    px: (f64, f64),
) -> Option<(DabSample, DVec3)> {
    let bvh = app.picking_bvh(id)?;
    let cam = app.camera?;
    let rect = app.viewport.last_rect?;
    let size = (rect.width() as f64, rect.height() as f64);
    let (origin, dir) = cam.ray(cam.target, size, px);
    // `shown_transform` only overrides the object's own transform while
    // the Transform tool is dragging or its commit is still in flight
    // (`None` the rest of the time, which is the normal case here: this
    // isn't `None` meaning "no transform").
    let transform = app
        .shown_transform(id)
        .or_else(|| app.session.document().object(id).map(|o| o.transform))?;
    let model = mf_render::model_matrix(&transform);
    let (face, t) = bvh.ray_pick(mesh, origin, dir, &model)?;
    let world = DVec3::from(origin) + DVec3::from(dir) * t;
    let local = model.inverse().transform_point3(world);
    let corners = mesh.face(face)?;
    let [a, b, c] = corners.map(|v| DVec3::from(mesh.corner_position(v)));
    // World-space corners (not just the normal) so a non-uniform scale
    // or shear in `model` doesn't need the usual inverse-transpose
    // correction: the cross product of transformed edges is always the
    // right world-space normal direction, just recomputed from points
    // instead of carried through as a vector.
    let [wa, wb, wc] = [a, b, c].map(|p| model.transform_point3(p));
    let world_normal = (wb - wa).cross(wc - wa);
    // Back-facing: the surface at the hit point faces the same way the
    // ray travels (away from the camera), not toward it.
    if world_normal.dot(DVec3::from(dir)) >= 0.0 {
        return None;
    }
    let bary = barycentric(local, a, b, c);
    Some((
        DabSample {
            face,
            bary,
            pressure: 1.0,
        },
        world,
    ))
}

/// The stroke's brush parameters (object space) from the panel settings,
/// the camera's view direction and `pivot_world` (the symmetry planes'
/// point, world space: the first dab's hit, kept for the whole stroke).
fn brush_params(
    cam_view_dir: DVec3,
    settings: &SculptSettings,
    transform: &[f64; 12],
    pivot_world: DVec3,
) -> Option<BrushParams> {
    let mut map = BTreeMap::new();
    map.insert(
        "brush".to_owned(),
        ParamValue::Text(settings.brush.word().to_owned()),
    );
    map.insert("radius".to_owned(), ParamValue::Float(settings.radius));
    map.insert("strength".to_owned(), ParamValue::Float(settings.strength));
    map.insert(
        "falloff".to_owned(),
        ParamValue::Text(falloff_word(settings.falloff).to_owned()),
    );
    map.insert(
        "symmetry_x".to_owned(),
        ParamValue::Bool(settings.symmetry[0]),
    );
    map.insert(
        "symmetry_y".to_owned(),
        ParamValue::Bool(settings.symmetry[1]),
    );
    map.insert(
        "symmetry_z".to_owned(),
        ParamValue::Bool(settings.symmetry[2]),
    );
    map.insert("pivot".to_owned(), ParamValue::Vec3(pivot_world.to_array()));
    map.insert(
        "view_dir".to_owned(),
        ParamValue::Vec3(cam_view_dir.to_array()),
    );
    map.insert("samples".to_owned(), ParamValue::Samples(Vec::new()));
    map.insert(
        "frozen".to_owned(),
        ParamValue::Faces(mf_spec::FaceList::default()),
    );
    map.insert(
        "detail_mode".to_owned(),
        ParamValue::Text(
            if settings.detail_on {
                "adaptive"
            } else {
                "off"
            }
            .to_owned(),
        ),
    );
    map.insert("detail".to_owned(), ParamValue::Float(settings.detail));
    map.insert(
        "stay_on_surface".to_owned(),
        ParamValue::Bool(settings.stay_on_surface),
    );
    let params = Params(map);
    mf_operations::sculpt::brush_params(&params, transform, &mf_operations::Target::WholeObject)
        // allow-swallow: a validation failure here just means no brush preview this frame; the real stroke goes through the full error path.
        .ok()
}

fn falloff_word(f: Falloff) -> &'static str {
    match f {
        Falloff::Smooth => "smooth",
        Falloff::Sphere => "sphere",
        Falloff::Sharp => "sharp",
        Falloff::Flat => "flat",
    }
}

fn brush_label(b: BrushKind) -> String {
    crate::i18n::tr(&format!("sculpt.brush.{}", b.word()), &[])
}

/// Each brush's glyph, matching what it does (owner: "Brush icons are
/// identical" — every brush used `icons::SCULPT`, so the rail couldn't be
/// told apart by icon). Order-independent: keyed on the brush itself so
/// `BRUSHES`' order can change without silently mismatching icons.
fn brush_icon(b: BrushKind) -> &'static str {
    match b {
        BrushKind::Smooth => icons::BRUSH_SMOOTH,
        BrushKind::Inflate => icons::BRUSH_INFLATE,
        BrushKind::Deflate => icons::BRUSH_DEFLATE,
        BrushKind::Flatten => icons::BRUSH_FLATTEN,
        BrushKind::Drag => icons::BRUSH_DRAG,
        BrushKind::Pinch => icons::BRUSH_PINCH,
        BrushKind::Crease => icons::BRUSH_CREASE,
        BrushKind::Carve => icons::BRUSH_CARVE,
    }
}

/// The digit key that switches straight to `b` while Sculpt is open, in
/// `BRUSHES`' order (1-8). `None` past the ninth brush (never happens: a
/// tenth brush would need a different scheme).
fn brush_shortcut(b: BrushKind) -> Option<egui::Key> {
    const KEYS: [egui::Key; 9] = [
        egui::Key::Num1,
        egui::Key::Num2,
        egui::Key::Num3,
        egui::Key::Num4,
        egui::Key::Num5,
        egui::Key::Num6,
        egui::Key::Num7,
        egui::Key::Num8,
        egui::Key::Num9,
    ];
    let i = BRUSHES.iter().position(|&k| k == b)?;
    KEYS.get(i).copied()
}

/// The shortcut's label (e.g. "1"), for the tooltip; empty if `b` has none.
fn brush_shortcut_label(b: BrushKind) -> String {
    brush_shortcut(b)
        .map(|k| crate::panels::shortcut_text(egui::Modifiers::NONE, k))
        // allow-swallow: documented: a brush without a shortcut has an empty label
        .unwrap_or_default()
}

/// The keys Sculpt takes while open: one digit per brush ([`brush_shortcut`]),
/// in `BRUSHES`' order, so this table can never drift from the tooltip's.
fn sculpt_key_table() -> Vec<(egui::Key, crate::tools::ToolKey)> {
    BRUSHES
        .iter()
        .filter_map(|&b| Some((brush_shortcut(b)?, crate::tools::ToolKey::Brush(b))))
        .collect()
}

fn sculpt_keys(_app: &MeshFixerApp) -> &'static [(egui::Key, crate::tools::ToolKey)] {
    use std::sync::OnceLock;
    static KEYS: OnceLock<Vec<(egui::Key, crate::tools::ToolKey)>> = OnceLock::new();
    KEYS.get_or_init(sculpt_key_table)
}

/// Sculpt's key handling: switch brush.
fn sculpt_on_key(app: &mut MeshFixerApp, key: crate::tools::ToolKey) {
    if let crate::tools::ToolKey::Brush(b) = key {
        app.sculpt.brush = b;
    }
}

fn falloff_label(f: Falloff) -> String {
    crate::i18n::tr(&format!("sculpt.falloff.{}", falloff_word(f)), &[])
}

// ---------------------------------------------------------------------
// The stroke's life cycle.
// ---------------------------------------------------------------------

/// Starts a stroke at `px` (a view point), if it lands on the active
/// object's surface.
fn begin_stroke(app: &mut MeshFixerApp) {
    let Some(px) = app.sculpt_hover else { return };
    let Some(id) = app.active else { return };
    let Some(mesh) = app.active_mesh().cloned() else {
        return;
    };
    let Some((dab, world)) = surface_point(app, id, &mesh, px) else {
        return;
    };
    let Some(bvh) = app.picking_bvh(id) else {
        return;
    };
    let transform = app.shown_transform(id).unwrap_or_else(|| {
        app.session
            .document()
            .object(id)
            .map(|o| o.transform)
            // allow-swallow: id is checked live just above (picking_bvh/active_mesh); a vanished object falls back to identity rather than panicking this frame.
            .unwrap_or_default()
    });
    let Some(cam) = app.camera else { return };
    let settings = app.sculpt.clone();
    let Some(brush) = brush_params(cam.view_dir(), &settings, &transform, world) else {
        return;
    };
    let radius_world = brush.radius * determinant_cbrt(&transform);
    let index = SculptIndex::from_bvh(bvh);
    let mut state = StrokeState::new(&mesh, index, brush);
    let ctx = mf_runtime::RunContext::none();
    let Ok(_) = state.apply(&dab, &ctx) else {
        return;
    };
    let preview = state.preview_mesh();
    let vertices = MeshVertices::build(&preview);
    // `GpuMesh` isn't `Clone` (its chunks are GPU buffers): the stroke's
    // own copy starts as a fresh upload of the whole mesh (one-time cost,
    // paid once per stroke, not per dab), rather than sharing the
    // displayed revision's buffers. From here, `GpuMesh::sync` against
    // this stroke's own `vertices` uploads only each dab's touched
    // chunks.
    let gpu = mf_render::GpuMesh::from_vertices(&app.gpu_device(), &vertices);
    let render = std::sync::Arc::new(crate::worker::RenderMesh::from_live(
        &preview, &vertices, gpu,
    ));
    app.sculpt_stroke = Some(SculptStroke {
        object: id,
        state,
        samples: vec![dab],
        vertices,
        render,
        last_dab: Some(world),
        radius_world,
    });
}

/// `transform`'s mean scale: the cube root of its linear part's
/// determinant's magnitude, as `mf_operations::sculpt::brush_params` uses
/// to convert a world radius to the object's own space.
fn determinant_cbrt(transform: &[f64; 12]) -> f64 {
    let m = glam::DMat3::from_cols(
        DVec3::new(transform[0], transform[1], transform[2]),
        DVec3::new(transform[3], transform[4], transform[5]),
        DVec3::new(transform[6], transform[7], transform[8]),
    );
    m.determinant().abs().cbrt()
}

/// Continues the open stroke to `px`, sampling once the pointer has moved
/// far enough (dab spacing, a fraction of the radius) since the last dab.
fn continue_stroke(app: &mut MeshFixerApp, px: (f64, f64)) {
    let Some(mut stroke) = app.sculpt_stroke.take() else {
        return;
    };
    let result = (|| {
        let mesh = stroke.state.preview_mesh();
        let (dab, world) = surface_point(app, stroke.object, &mesh, px)?;
        if let Some(last) = stroke.last_dab
            && (world - last).length() < DAB_SPACING * stroke.radius_world
        {
            return None;
        }
        let ctx = mf_runtime::RunContext::none();
        // allow-swallow: a stale/invalid preview dab (e.g. the face it landed on moved since last frame) is a no-op this frame, same as the spacing check above; the committed stroke is validated and refused for real through the command path.
        stroke.state.apply(&dab, &ctx).ok()?;
        stroke.samples.push(dab);
        stroke.last_dab = Some(world);
        let next_mesh = stroke.state.preview_mesh();
        stroke.vertices = MeshVertices::build_from(Some((&mesh, &stroke.vertices)), &next_mesh);
        let (device, queue) = (app.gpu_device(), app.gpu_queue());
        // Usually the only `Arc` to it (the previous frame's borrow in
        // `object_views` doesn't outlive that frame): synced in place, so
        // only this dab's touched chunks re-upload (MF-RENDER-003). If a
        // frame's draw still holds a clone when the next dab runs, fall
        // back to a fresh upload of the whole mesh for this one dab
        // rather than panicking: correctness over the chunk-reuse
        // optimization that one time.
        match std::sync::Arc::get_mut(&mut stroke.render) {
            Some(render) => {
                render.gpu.sync(&device, &queue, &stroke.vertices);
                render.mesh = next_mesh;
                render.bounds = stroke.vertices.bounds;
                render.origin = stroke.vertices.origin;
                if app.sculpt_show_changes
                    && let Some((id, baseline)) = &app.sculpt_baseline
                    && *id == stroke.object
                {
                    let states = sculpt_changes_face_states(baseline, &render.mesh);
                    render.gpu.set_face_states(&queue, &states);
                }
            }
            None => {
                let gpu = mf_render::GpuMesh::from_vertices(&device, &stroke.vertices);
                stroke.render = std::sync::Arc::new(crate::worker::RenderMesh::from_live(
                    &next_mesh,
                    &stroke.vertices,
                    gpu,
                ));
            }
        }
        Some(())
    })();
    // allow-swallow: the closure's job is its side effects on `stroke`; every early-return path above is a no-op frame, not a dropped error.
    let _ = result;
    app.sculpt_stroke = Some(stroke);
}

/// Ends whatever stroke is open: committed on [`Close::Commit`] (releasing
/// the pointer, or closing the tool while a stroke was left mid-drag),
/// dropped on [`Close::Cancel`] (Esc).
fn end_stroke(app: &mut MeshFixerApp, close: Close) {
    let Some(stroke) = app.sculpt_stroke.take() else {
        return;
    };
    if close == Close::Cancel || stroke.samples.is_empty() {
        return;
    }
    commit_stroke(app, stroke);
}

/// Commits `stroke`: replays its recorded samples through
/// `mesh.sculpt_stroke` (one undo step), the pivot and view direction
/// fixed to what the stroke was drawn with (the first dab's hit point and
/// the camera at the time), so a replay is identical however the camera
/// moves meanwhile.
fn commit_stroke(app: &mut MeshFixerApp, stroke: SculptStroke) {
    let Some(cam) = app.camera else { return };
    let Some(world) = stroke.last_dab else { return };
    let settings = app.sculpt.clone();
    // Keep the stroke's last live mesh on screen past release: the replay
    // through `mesh.sculpt_stroke` runs on a worker (this frame only
    // starts the job), so there's a real gap before the committed
    // revision's own GPU mesh and analysis are ready. Without this, that
    // gap shows the pre-stroke mesh again (a revert) and then "Updating
    // view…" over it — the flicker the owner saw. `refresh_analysis`
    // clears this once the real revision has swapped in, in that same
    // frame (owner: "swap in a single frame").
    app.sculpt_pending = Some((stroke.object, stroke.render.clone()));
    let samples: Vec<[f64; 5]> = stroke.samples.iter().map(DabSample::to_array).collect();
    let params: BTreeMap<String, String> = BTreeMap::from([
        ("brush".to_owned(), settings.brush.word().to_owned()),
        ("radius".to_owned(), settings.radius.to_string()),
        ("strength".to_owned(), settings.strength.to_string()),
        (
            "falloff".to_owned(),
            falloff_word(settings.falloff).to_owned(),
        ),
        ("symmetry_x".to_owned(), settings.symmetry[0].to_string()),
        ("symmetry_y".to_owned(), settings.symmetry[1].to_string()),
        ("symmetry_z".to_owned(), settings.symmetry[2].to_string()),
        ("pivot".to_owned(), vec3(world)),
        ("view_dir".to_owned(), vec3(cam.view_dir())),
        ("samples".to_owned(), samples_text(&samples)),
        (
            "detail_mode".to_owned(),
            (if settings.detail_on {
                "adaptive"
            } else {
                "off"
            })
            .to_owned(),
        ),
        ("detail".to_owned(), settings.detail.to_string()),
        (
            "stay_on_surface".to_owned(),
            settings.stay_on_surface.to_string(),
        ),
    ]);
    app.run_invocation(
        mf_command::Invocation {
            op_id: "mesh.sculpt_stroke".into(),
            target: stroke.object,
            selection: None,
            operands: Vec::new(),
            raw_params: params,
        },
        t!("sculpt.apply"),
    );
}

fn vec3(v: DVec3) -> String {
    format!("{},{},{}", v.x, v.y, v.z)
}

fn samples_text(samples: &[[f64; 5]]) -> String {
    samples
        .iter()
        .map(|d| d.iter().map(f64::to_string).collect::<Vec<_>>().join(","))
        .collect::<Vec<_>>()
        .join(";")
}

// ---------------------------------------------------------------------
// Viewport input and the brush cursor.
// ---------------------------------------------------------------------

fn viewport_input(app: &mut MeshFixerApp, input: &crate::shell::ToolInput) -> Input {
    app.sculpt_hover = input.hover;
    match input.pointer {
        Some(ToolPointer::Start { at, .. }) => {
            app.sculpt_hover = Some(at);
            begin_stroke(app);
            Input::Consumed
        }
        Some(ToolPointer::Move(at)) => {
            if app.sculpt_stroke.is_some() {
                continue_stroke(app, at);
            }
            Input::Consumed
        }
        Some(ToolPointer::End(at)) => {
            if app.sculpt_stroke.is_some() {
                continue_stroke(app, at);
                end_stroke(app, Close::Commit);
            }
            Input::Consumed
        }
        Some(ToolPointer::Click(_)) | None => Input::Pass,
    }
}

/// The brush cursor: a circle on the surface at the pointer, the brush's
/// world radius across (Step 1's `brush_cursor_is_a_hover_outline`).
pub fn cursor_overlay(app: &MeshFixerApp, ctx: &egui::Context) {
    if app.active_tool != SCULPT {
        return;
    }
    let (Some(rect), Some(cam), Some(id)) = (app.viewport.last_rect, app.camera, app.active) else {
        return;
    };
    let Some(px) = app.sculpt_hover else { return };
    let Some(mesh) = app.active_mesh().cloned() else {
        return;
    };
    let Some((_, world)) = surface_point(app, id, &mesh, px) else {
        return;
    };
    let transform = app
        .shown_transform(id)
        .or_else(|| app.session.document().object(id).map(|o| o.transform))
        // allow-swallow: id is checked live just above (active_mesh/surface_point); a vanished object falls back to identity rather than panicking this frame.
        .unwrap_or_default();
    let radius_world = app.sculpt.radius * determinant_cbrt(&transform);
    let view = (rect.width() as f64, rect.height() as f64);
    let normal = DVec3::from(cam.ray(cam.target, view, px).1) * -1.0;
    let u = normal.any_orthonormal_vector();
    let v = normal.cross(u);
    let at = |p: DVec3| {
        mf_render::screen_position(&cam, cam.target, view, p.to_array())
            .map(|(x, y)| rect.left_top() + egui::vec2(x as f32, y as f32))
    };
    let pts: Vec<egui::Pos2> = (0..48)
        .filter_map(|i| {
            let a = std::f64::consts::TAU * i as f64 / 48.0;
            at(world + (u * a.cos() + v * a.sin()) * radius_world)
        })
        .collect();
    if pts.len() == 48 {
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Middle,
            egui::Id::unique("sculpt cursor"),
        ));
        painter.with_clip_rect(rect).add(egui::Shape::closed_line(
            pts,
            egui::Stroke::new(1.5, theme::HOVER.gamma_multiply(0.8)),
        ));
    }
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

fn panel(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    widgets::body(ui, t!("sculpt.help"));
    let units = crate::defects::unit_symbol(app.session.document().units());

    section(ui, &t!("sculpt.section.brush"));
    ui.horizontal_wrapped(|ui| {
        for b in BRUSHES {
            let label = brush_label(b);
            let shortcut = brush_shortcut_label(b);
            let tip = if shortcut.is_empty() {
                label.clone()
            } else {
                format!("{label} ({shortcut})")
            };
            if widgets::icon_button(ui, brush_icon(b), &label, Some(app.sculpt.brush == b), 28.0)
                .on_hover_text(tip)
                .clicked()
            {
                app.sculpt.brush = b;
            }
        }
    });

    section(ui, &t!("sculpt.section.radius", unit = units));
    ui.add(
        egui::Slider::new(&mut app.sculpt.radius, 0.1..=200.0)
            .logarithmic(true)
            .suffix(format!(" {units}"))
            .max_decimals(2),
    );

    section(ui, &t!("sculpt.section.strength"));
    ui.add(egui::Slider::new(&mut app.sculpt.strength, 0.0..=1.0).max_decimals(2));

    section(ui, &t!("sculpt.section.falloff"));
    let labels: Vec<String> = FALLOFFS.iter().map(|&f| falloff_label(f)).collect();
    let options: Vec<(&str, &str, bool)> = labels
        .iter()
        .map(|l| (l.as_str(), l.as_str(), true))
        .collect();
    let current = FALLOFFS
        .iter()
        .position(|&f| f == app.sculpt.falloff)
        // allow-swallow: FALLOFFS is the exhaustive, fixed list of Falloff values, so app.sculpt.falloff is always found; the default is unreachable.
        .unwrap_or(0);
    if let (Some(i), _) = widgets::segmented(ui, &options, current) {
        app.sculpt.falloff = FALLOFFS[i];
    }
    ui.checkbox(
        &mut app.sculpt.stay_on_surface,
        t!("sculpt.stay_on_surface"),
    )
    .on_hover_text(t!("sculpt.stay_on_surface.tip"));

    section(ui, &t!("sculpt.section.symmetry"));
    ui.horizontal(|ui| {
        for (axis, label) in [
            (0usize, t!("sculpt.symmetry.x")),
            (1, t!("sculpt.symmetry.y")),
            (2, t!("sculpt.symmetry.z")),
        ] {
            let mut on = app.sculpt.symmetry[axis];
            if ui.checkbox(&mut on, label).changed() {
                app.sculpt.symmetry[axis] = on;
            }
        }
    });

    section(ui, &t!("sculpt.section.detail"));
    ui.checkbox(&mut app.sculpt.detail_on, t!("sculpt.detail"))
        .on_hover_text(t!("sculpt.detail.tip"));
    if app.sculpt.detail_on {
        ui.add(egui::Slider::new(&mut app.sculpt.detail, 0.05..=1.0).max_decimals(2));
    }

    section(ui, &t!("sculpt.section.overlay"));
    ui.checkbox(&mut app.sculpt_show_changes, t!("sculpt.show_changes"))
        .on_hover_text(t!("sculpt.show_changes.tip"));
}

// ---------------------------------------------------------------------
// "Show sculpt changes": tints vertices by how far they've moved since
// the tool opened (the Sculpt panel's overlay checkbox).
// ---------------------------------------------------------------------

/// A moved corner's tier: how far it's moved, as a fraction of `scale`
/// (the mesh's own bounding-box diagonal, so the thresholds mean the same
/// thing on a 5 mm part and a 500 mm one).
fn sculpt_change_tier(moved: f64, scale: f64) -> u8 {
    if scale <= 0.0 || !scale.is_finite() {
        return 0;
    }
    let t = moved / scale;
    if t > 0.03 {
        mf_render::FaceStates::SCULPT_CHANGED_HIGH
    } else if t > 0.01 {
        mf_render::FaceStates::SCULPT_CHANGED_MED
    } else if t > 0.002 {
        mf_render::FaceStates::SCULPT_CHANGED_LOW
    } else {
        0
    }
}

/// "Show sculpt changes": every live face of `current` whose corners have
/// moved enough from `baseline` (the mesh as of when Sculpt opened) to
/// read a tier, paired with that tier (the max of its corners'). A vertex
/// `baseline` doesn't have (dynamic detail created it mid-stroke) reads
/// as the top tier: it's new since the tool opened, the most a vertex can
/// have "changed". O(current's faces).
pub fn sculpt_changed_faces(baseline: &Mesh, current: &Mesh) -> Vec<(u32, u8)> {
    let scale = current
        .bounding_box()
        .map(|bb| {
            let d = std::array::from_fn::<f64, 3, _>(|k| bb.max[k] - bb.min[k]);
            (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
        })
        .unwrap_or(0.0);
    let corner_tier = |v: u32| -> u8 {
        let Some(now) = current.position(mf_geometry::VertexId(v)) else {
            return 0;
        };
        match baseline.position(mf_geometry::VertexId(v)) {
            Some(before) => {
                let d = (0..3)
                    .map(|k| (now[k] - before[k]).powi(2))
                    .sum::<f64>()
                    .sqrt();
                sculpt_change_tier(d, scale)
            }
            // A vertex the baseline doesn't have: created after the tool
            // opened (dynamic detail's remeshing).
            None => mf_render::FaceStates::SCULPT_CHANGED_HIGH,
        }
    };
    current
        .faces()
        .filter_map(|(f, t)| {
            // allow-swallow: a triangle always has three corners, so max() is Some
            let tier = t.iter().map(|&v| corner_tier(v)).max().unwrap_or(0);
            (tier > 0).then_some((f.0, tier))
        })
        .collect()
}

/// As [`sculpt_changed_faces`], as a standalone [`mf_render::FaceStates`]
/// (the live stroke's own GPU mesh, which carries no other overlay).
pub fn sculpt_changes_face_states(baseline: &Mesh, current: &Mesh) -> mf_render::FaceStates {
    let mut flagged = sculpt_changed_faces(baseline, current);
    mf_render::FaceStates::from_flagged(current, &mut flagged)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // tests may panic on failure
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// Owner (2026-11-11): "Brush icons are identical." Every brush must
    /// draw its own glyph, not all fall back to the tool's own
    /// (`icons::SCULPT`).
    #[test]
    fn brush_icons_are_unique() {
        let icons: HashSet<&str> = BRUSHES.iter().map(|&b| brush_icon(b)).collect();
        assert_eq!(
            icons.len(),
            BRUSHES.len(),
            "every brush must have its own icon, matching what it does"
        );
    }

    /// No brush should silently reuse the Sculpt tool's own rail icon:
    /// that was the bug (every button drew `icons::SCULPT`).
    #[test]
    fn no_brush_reuses_the_tool_icon() {
        for b in BRUSHES {
            assert_ne!(
                brush_icon(b),
                icons::SCULPT,
                "{b:?} must not fall back to the tool's own icon"
            );
        }
    }

    /// Each brush gets its own digit shortcut (1-8, in `BRUSHES`' order),
    /// and the tooltip can render it (brief: "The tooltip shows the brush
    /// name and its shortcut").
    #[test]
    fn brush_shortcuts_are_unique_and_present() {
        let mut seen = HashSet::new();
        for b in BRUSHES {
            let key = brush_shortcut(b).expect("every brush has a shortcut");
            assert!(
                seen.insert(key),
                "{b:?}'s shortcut collides with another brush's"
            );
            assert!(!brush_shortcut_label(b).is_empty());
        }
    }

    /// `sculpt_keys`' table agrees with [`brush_shortcut`]: pressing the
    /// labelled key is wired to the same brush the tooltip promises.
    #[test]
    fn sculpt_keys_match_brush_shortcuts() {
        let keys = sculpt_key_table();
        for b in BRUSHES {
            let want = brush_shortcut(b).expect("every brush has a shortcut");
            let found = keys
                .iter()
                .find(|(_, k)| *k == crate::tools::ToolKey::Brush(b))
                .map(|(k, _)| *k);
            assert_eq!(
                found,
                Some(want),
                "{b:?}'s wired key must match its tooltip shortcut"
            );
        }
    }

    /// "Show sculpt changes": an unmoved mesh has no changed faces at all.
    #[test]
    fn an_unchanged_mesh_has_no_sculpt_changed_faces() {
        let mesh = mf_fixtures::grid(6);
        assert!(sculpt_changed_faces(&mesh, &mesh).is_empty());
    }

    /// A small, a moderate and a large move (as fractions of the mesh's
    /// own bounding-box diagonal) land in the low, medium and high tiers
    /// respectively, and every face touching the moved vertex is flagged.
    #[test]
    fn moves_land_in_the_right_tier_by_size() {
        let mesh = mf_fixtures::grid(6);
        let diag = {
            let bb = mesh.bounding_box().unwrap();
            let d: [f64; 3] = std::array::from_fn(|k| bb.max[k] - bb.min[k]);
            (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
        };
        let moved = |frac: f64| {
            let mut edit = mesh.edit();
            let p = mesh.position(mf_geometry::VertexId(0)).unwrap();
            edit.move_vertex(mf_geometry::VertexId(0), [p[0] + frac * diag, p[1], p[2]])
                .unwrap();
            edit.finish().unwrap().0
        };
        use mf_render::FaceStates;
        for (frac, want) in [
            (0.001, 0u8), // under the low threshold: unchanged
            (0.005, FaceStates::SCULPT_CHANGED_LOW),
            (0.02, FaceStates::SCULPT_CHANGED_MED),
            (0.05, FaceStates::SCULPT_CHANGED_HIGH),
        ] {
            let after = moved(frac);
            let changed = sculpt_changed_faces(&mesh, &after);
            if want == 0 {
                assert!(
                    changed.is_empty(),
                    "{frac}: expected no change, got {changed:?}"
                );
            } else {
                assert!(!changed.is_empty(), "{frac}: expected some tier, got none");
                assert!(
                    changed.iter().all(|&(_, tier)| tier == want),
                    "{frac}: expected every flagged face at tier {want}, got {changed:?}"
                );
                // Every live face incident to vertex 0 is flagged (the max
                // of its corners' tiers, and only vertex 0 moved).
                let incident: HashSet<u32> = after
                    .faces()
                    .filter(|(_, t)| t.contains(&0))
                    .map(|(f, _)| f.0)
                    .collect();
                let flagged: HashSet<u32> = changed.iter().map(|&(f, _)| f).collect();
                assert_eq!(flagged, incident, "{frac}: wrong faces flagged");
            }
        }
    }

    /// A vertex the baseline doesn't have (created by dynamic detail's
    /// remeshing mid-stroke) reads as the top tier: new since the tool
    /// opened is the most a vertex can have "changed".
    #[test]
    fn a_vertex_new_since_the_baseline_reads_as_the_top_tier() {
        let before = mf_fixtures::grid(6);
        // A face split (a new vertex at its centroid): present in `after`
        // but not `before`.
        let mut edit = before.edit();
        let (_, t) = before.faces().next().unwrap();
        let corners: Vec<[f64; 3]> = t
            .iter()
            .map(|&v| before.position(mf_geometry::VertexId(v)).unwrap())
            .collect();
        let centroid = std::array::from_fn(|k| corners.iter().map(|c| c[k]).sum::<f64>() / 3.0);
        let new_v = edit.add_vertex(centroid);
        edit.remove_face(mf_geometry::FaceId(0)).unwrap();
        edit.add_face([t[0], t[1], new_v.0], mf_geometry::Sources::default())
            .unwrap();
        edit.add_face([t[1], t[2], new_v.0], mf_geometry::Sources::default())
            .unwrap();
        edit.add_face([t[2], t[0], new_v.0], mf_geometry::Sources::default())
            .unwrap();
        let after = edit.finish().unwrap().0;
        let changed = sculpt_changed_faces(&before, &after);
        let new_faces: HashSet<u32> = after
            .faces()
            .filter(|(_, t)| t.contains(&new_v.0))
            .map(|(f, _)| f.0)
            .collect();
        assert!(!new_faces.is_empty());
        for f in &new_faces {
            let tier = changed.iter().find(|&&(cf, _)| cf == *f).map(|&(_, t)| t);
            assert_eq!(
                tier,
                Some(mf_render::FaceStates::SCULPT_CHANGED_HIGH),
                "a face touching a vertex new since the baseline must read the top tier"
            );
        }
    }
}
