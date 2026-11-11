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
use mf_geometry::{FaceId, Mesh};
use mf_operations::sculpt::{BrushKind, BrushParams, DabSample, Falloff, SculptIndex, StrokeState};
use mf_render::MeshVertices;
use mf_spec::{ParamValue, Params};

use crate::MeshFixerApp;
use crate::shell::{Availability, Category, Close, Input, RegisterError, Shell, Tool as T};
use crate::viewport::ToolPointer;
use crate::widgets::{self, Kind};
use crate::{icons, t, theme};

pub const SCULPT: &str = "sculpt";

/// Every brush, in the order the panel lists them.
const BRUSHES: [BrushKind; 8] = BrushKind::ALL;
/// Every falloff, in the order the panel lists them.
const FALLOFFS: [Falloff; 4] = [Falloff::Smooth, Falloff::Sphere, Falloff::Sharp, Falloff::Flat];
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
        }
    }
}

/// A stroke in progress (one undo step when it commits).
pub struct SculptStroke {
    pub object: ObjectId,
    /// The revision the stroke started from: what `mesh.sculpt_stroke`
    /// replays `samples` against.
    base: Mesh,
    state: StrokeState,
    samples: Vec<DabSample>,
    /// The vertices `gpu` was last synced to (this stroke's own running
    /// copy: not the object's displayed `RenderMesh`, which keeps none).
    vertices: MeshVertices,
    gpu: mf_render::GpuMesh,
    /// Where the pointer last dabbed, in world space: the next dab only
    /// samples once the pointer has moved [`DAB_SPACING`] of the radius.
    last_dab: Option<DVec3>,
    /// The brush's world-space radius (dab spacing is a fraction of it).
    radius_world: f64,
}

impl SculptStroke {
    /// What the view should show for this stroke right now.
    pub fn render(&self, mesh: &Mesh) -> RenderMeshLive {
        RenderMeshLive {
            mesh: mesh.clone(),
            bounds: self.vertices.bounds,
            origin: self.vertices.origin,
            gpu: self.gpu.clone(),
        }
    }
}

/// What [`crate::worker::RenderMesh::from_live`] needs: cheap to build
/// per frame from the stroke (no CPU copy of the vertices, like
/// `RenderMesh` itself).
pub struct RenderMeshLive {
    pub mesh: Mesh,
    pub bounds: Option<mf_geometry::Aabb>,
    pub origin: DVec3,
    pub gpu: mf_render::GpuMesh,
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
        viewport_input,
        hides_defects: |app| app.sculpt_stroke.is_some(),
        on_close: |app, close| end_stroke(app, close),
        ..T::new(SCULPT, "tool.sculpt.name", icons::SCULPT, Category::Shape, 30)
    })
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
    let transform = app.shown_transform(id)?;
    let model = mf_render::model_matrix(&transform);
    let (face, t) = bvh.ray_pick(mesh, origin, dir, &model)?;
    let world = DVec3::from(origin) + DVec3::from(dir) * t;
    let local = model.inverse().transform_point3(world);
    let corners = mesh.face(face)?;
    let [a, b, c] = corners.map(|v| DVec3::from(mesh.corner_position(v)));
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
        ParamValue::Text(if settings.detail_on { "adaptive" } else { "off" }.to_owned()),
    );
    map.insert("detail".to_owned(), ParamValue::Float(settings.detail));
    let params = Params(map);
    mf_operations::sculpt::brush_params(&params, transform, &mf_operations::Target::WholeObject).ok()
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
    t!(&format!("sculpt.brush.{}", b.word()))
}

fn falloff_label(f: Falloff) -> String {
    t!(&format!("sculpt.falloff.{}", falloff_word(f)))
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
    let Some(bvh) = app.picking_bvh(id) else { return };
    let transform = app.shown_transform(id).unwrap_or_else(|| {
        app.session
            .document()
            .object(id)
            .map(|o| o.transform)
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
    let Some(mut gpu) = app.active_gpu_mesh() else {
        return;
    };
    gpu.sync(&app.gpu_device(), &app.gpu_queue(), &vertices);
    app.sculpt_stroke = Some(SculptStroke {
        object: id,
        base: mesh,
        state,
        samples: vec![dab],
        vertices,
        gpu,
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
        stroke.state.apply(&dab, &ctx).ok()?;
        stroke.samples.push(dab);
        stroke.last_dab = Some(world);
        let next_mesh = stroke.state.preview_mesh();
        stroke.vertices = MeshVertices::build_from(Some((&mesh, &stroke.vertices)), &next_mesh);
        stroke
            .gpu
            .sync(&app.gpu_device(), &app.gpu_queue(), &stroke.vertices);
        Some(())
    })();
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

fn commit_stroke(app: &mut MeshFixerApp, stroke: SculptStroke) {
    let Some(transform) = app
        .session
        .document()
        .object(stroke.object)
        .map(|o| o.transform)
    else {
        return;
    };
    let Some(cam) = app.camera else { return };
    let Some(world) = stroke.last_dab else { return };
    let settings = app.sculpt.clone();
    let Some(brush) = brush_params(cam.view_dir(), &settings, &transform, world) else {
        return;
    };
    let mut map = BTreeMap::new();
    map.insert(
        "brush".to_owned(),
        ParamValue::Text(settings.brush.word().to_owned()),
    );
    map.insert("radius".to_owned(), settings.radius.to_string());
    let _ = brush;
    let samples: Vec<[f64; 5]> = stroke.samples.iter().map(DabSample::to_array).collect();
    let pivot = stroke.last_dab.unwrap_or(DVec3::ZERO);
    let params: BTreeMap<String, String> = BTreeMap::from([
        ("brush".to_owned(), settings.brush.word().to_owned()),
        ("radius".to_owned(), settings.radius.to_string()),
        ("strength".to_owned(), settings.strength.to_string()),
        ("falloff".to_owned(), falloff_word(settings.falloff).to_owned()),
        ("symmetry_x".to_owned(), settings.symmetry[0].to_string()),
        ("symmetry_y".to_owned(), settings.symmetry[1].to_string()),
        ("symmetry_z".to_owned(), settings.symmetry[2].to_string()),
        ("pivot".to_owned(), vec3(pivot)),
        ("view_dir".to_owned(), vec3(cam.view_dir())),
        ("samples".to_owned(), samples_text(&samples)),
        (
            "detail_mode".to_owned(),
            (if settings.detail_on { "adaptive" } else { "off" }).to_owned(),
        ),
        ("detail".to_owned(), settings.detail.to_string()),
    ]);
    let _ = map;
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
    let (Some(rect), Some(cam), Some(id)) = (app.viewport.last_rect, app.camera, app.active)
    else {
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
            egui::Id::new("sculpt cursor"),
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
            if widgets::icon_button(
                ui,
                icons::SCULPT,
                &label,
                Some(app.sculpt.brush == b),
                28.0,
            )
            .on_hover_text(label.clone())
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
    let options: Vec<(&str, &str, bool)> = labels.iter().map(|l| (l.as_str(), l.as_str(), true)).collect();
    let current = FALLOFFS
        .iter()
        .position(|&f| f == app.sculpt.falloff)
        .unwrap_or(0);
    if let (Some(i), _) = widgets::segmented(ui, &options, current) {
        app.sculpt.falloff = FALLOFFS[i];
    }

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
}
