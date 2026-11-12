//! The Shape tools on the rail (UI plan §3, step 5): Split
//! (`mesh.split_plane`, every selected object), Combine (`mesh.combine`:
//! join the selected solids and cut the selected holes, Tinkercad-style;
//! `mesh.boolean` intersect under More) and Make solid
//! (`mesh.make_solid`), registered through `shell::Tool` like every other
//! tool: an icon, a one-level panel with ONE primary action, plain-language
//! refusals with the next action as a button, and the job pool's progress
//! and Cancel (the status bar).

use std::collections::BTreeMap;

use eframe::egui::{self, RichText};
use glam::DVec3;
use mf_command::{Invocation, ObjectId};
use mf_render::{Gizmo, GizmoMode, Handle, Snap};

use crate::MeshFixerApp;
use crate::shell::{Availability, Category, Input, RegisterError, Shell, Tool as T};
use crate::widgets::{self, Kind};
use crate::{icons, t, theme};

pub const SPLIT: &str = "split";
pub const COMBINE: &str = "combine";
pub const MAKE_SOLID: &str = "make_solid";

/// The operation a Shape tool runs (its refusals show in its panel).
pub fn op_of(tool: &str) -> Option<&'static str> {
    match tool {
        SPLIT => Some("mesh.split_plane"),
        COMBINE => Some("mesh.combine"),
        MAKE_SOLID => Some("mesh.make_solid"),
        _ => None,
    }
}

/// As [`op_of`], for a tool whose panel can run more than one operation
/// (Parts: placing with `object.add_part`, then Join to scan with
/// `mesh.boolean`; Connectors: placing, then Merge with
/// `connector.join`): every op id whose refusal that tool's panel knows
/// how to show, in the order its own `refusal_card_for` calls check
/// them.
pub(crate) fn ops_of(tool: &str) -> &'static [&'static str] {
    match tool {
        crate::parts_tool::PARTS => &["object.add_part", "object.add_shape", "mesh.boolean"],
        crate::connector_tool::CONNECTORS => &[
            "object.add_part",
            "object.add_shape",
            "connector.join",
            "object.attach",
            "object.set_shape_params",
        ],
        crate::connect_tool::CONNECT => &["split.place_connectors", "object.add_shape"],
        crate::tool_panels::SELECT | crate::transform_tool::ID => &["object.set_shape_params"],
        _ => &[],
    }
}

/// A refusal from an operation, shown in its tool's panel in plain words,
/// with the next action as a button where there is one.
#[derive(Debug, Clone, PartialEq)]
pub struct Refusal {
    pub op_id: String,
    pub code: String,
    pub message: String,
}

/// Split's state: the cut plane (as a placement: its translation is a
/// point on the plane, its linear part turns +Z onto the plane's normal),
/// the gizmo's mode and drag, and the settings.
#[derive(Debug, Clone)]
pub struct SplitState {
    pub plane: [f64; 12],
    /// Move (the plane's point) or Rotate (tilt it).
    pub mode: GizmoMode,
    pub drag: Option<(mf_render::Drag, [f64; 12])>,
    pub hover: Option<Handle>,
    /// "both", "above" or "below".
    pub keep: String,
    pub gap: f64,
    /// The targets ([`MeshFixerApp::selected_objects`]) the plane was last
    /// placed for (owner 2026-11-11: centred on the whole selection, not
    /// just the active object — `mesh.split_plane` cuts every selected
    /// object in one command). A different selection places it again.
    pub placed_for: Option<Vec<ObjectId>>,
    /// The section view (on by default): the part above the plane
    /// ghosted, the part below shaded, the cut drawn at the plane.
    pub section: bool,
}

impl Default for SplitState {
    fn default() -> SplitState {
        SplitState {
            plane: mf_render::compose(DVec3::ZERO, DVec3::ZERO, DVec3::ONE, DVec3::ZERO),
            mode: GizmoMode::Move,
            drag: None,
            hover: None,
            keep: "both".into(),
            gap: 0.0,
            placed_for: None,
            section: true,
        }
    }
}

impl SplitState {
    /// A point on the plane (world).
    pub fn point(&self) -> DVec3 {
        DVec3::new(self.plane[9], self.plane[10], self.plane[11])
    }

    /// The plane's unit normal (world): the top part is on its side.
    pub fn normal(&self) -> DVec3 {
        DVec3::new(self.plane[6], self.plane[7], self.plane[8]).normalize_or(DVec3::Z)
    }

    /// The tilt as (about X, about Y) degrees.
    fn tilt(&self) -> (f64, f64) {
        match mf_render::decompose(&self.plane) {
            mf_render::Decomposed::Trs { rotate_deg_xyz, .. } => {
                (rotate_deg_xyz.x, rotate_deg_xyz.y)
            }
            mf_render::Decomposed::Sheared => (0.0, 0.0),
        }
    }

    fn set(&mut self, point: DVec3, tilt: (f64, f64)) {
        self.plane = mf_render::compose(
            point,
            DVec3::new(tilt.0, tilt.1, 0.0),
            DVec3::ONE,
            DVec3::ZERO,
        );
    }

    /// The names a cut at this orientation gives its two sides (owner
    /// 2026-11-12: "above"/"below" means nothing for a vertical or
    /// tilted cut — the side "the normal points to" isn't what either
    /// word promises). Side A is always the side the plane's normal
    /// points to (`keep: above`'s side in `mesh.split_plane`); side B is
    /// the other (`keep: below`'s side). The words only change; the
    /// engine's `keep` values and their meaning (which side the normal
    /// points to) don't. The same orientation rule names the parts
    /// `mesh.split_plane` creates ([`mf_operations::split_names`]), so a
    /// part's name always matches the button that kept it.
    pub fn side_names(&self) -> mf_operations::SideNames {
        mf_operations::SideNames::for_normal(self.normal().to_array())
    }
}

/// The i18n keys for [`mf_operations::SideNames`]'s two sides: `(side
/// a's label key, side b's label key)`, for `i18n::tr`.
pub fn side_name_labels(sides: mf_operations::SideNames) -> (&'static str, &'static str) {
    use mf_operations::SideNames;
    match sides {
        SideNames::TopBottom => ("split.side.top", "split.side.bottom"),
        SideNames::LeftRight => ("split.side.right", "split.side.left"),
        SideNames::FrontBack => ("split.side.back", "split.side.front"),
        SideNames::SideAB => ("split.side.a", "split.side.b"),
    }
}

/// Combine's settings: whether the holes are kept (Combine acts on the
/// objects selected together, [`MeshFixerApp::selected_objects`], each by
/// its Solid/Hole role).
#[derive(Debug, Default)]
pub struct CombineState {
    pub keep_holes: bool,
}

/// Split's Alignment section (Phase 1 Task 13, D12): holes and dowels at
/// the cut, from the fit profile's defaults.
#[derive(Debug, Clone)]
pub struct AlignmentState {
    /// "none", "dowels", "attached" or "dovetail" (the spec's words).
    pub pegs: String,
    pub peg_shape: mf_operations::PegShape,
    pub hole_diameter: f64,
    pub hole_depth_above: f64,
    pub hole_depth_below: f64,
    /// 0: automatic (the operation places as many as fit).
    pub peg_count: u32,
    pub peg_clearance: f64,
    pub length_allowance: f64,
    pub min_wall: f64,
}

impl Default for AlignmentState {
    fn default() -> AlignmentState {
        AlignmentState {
            pegs: "none".into(),
            peg_shape: mf_operations::PegShape::Round,
            hole_diameter: 5.0,
            hole_depth_above: 6.0,
            hole_depth_below: 6.0,
            peg_count: 0,
            peg_clearance: 0.2,
            length_allowance: 0.4,
            min_wall: 1.5,
        }
    }
}

/// What the Alignment preview was last asked for: everything
/// [`crate::worker::PegPreviewRequest`] carries but the `Mesh` (no
/// `PartialEq`, MF-DOC-001), so a frame with nothing new to ask doesn't
/// resubmit it.
#[derive(Debug, Clone, PartialEq)]
pub struct PegPreviewKey {
    pub object: ObjectId,
    pub transform: [f64; 12],
    pub plane_point: [f64; 3],
    pub plane_normal: [f64; 3],
    pub shape: mf_operations::PegShape,
    pub diameter: f64,
    pub depth_above: f64,
    pub depth_below: f64,
    pub clearance: f64,
    pub allowance: f64,
    pub count: u32,
    pub min_wall: f64,
}

/// Make solid's settings: the cell size (mm; `None` until the object's
/// default is known: 1/256 of its diagonal, as the operation chooses),
/// the offset, and whether to keep the surface where the grid puts it.
#[derive(Debug, Clone, Default)]
pub struct SolidState {
    pub cell: Option<f64>,
    pub offset: f64,
    pub smooth: bool,
    pub for_object: Option<ObjectId>,
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
        shortcut: Some(egui::Key::S),
        help: "tool.split.help",
        availability: needs_object,
        panel: Some(crate::split_flow::panel),
        claims: crate::split_flow::claims,
        viewport_input: crate::split_flow::input,
        on_open: |app| {
            app.clear_selection();
            app.refusal = None;
            app.split_flow.last_cut = None;
            place_plane(app, false);
        },
        on_close: |app, _| {
            app.split.drag = None;
            app.split.hover = None;
            app.split_flow.volume.drag = None;
            app.split_flow.volume.face_drag = None;
        },
        ..T::new(SPLIT, "tool.split.name", icons::SPLIT, Category::Shape, 0)
    })?;
    shell.register_tool(T {
        shortcut: Some(egui::Key::C),
        help: "tool.combine.help",
        availability: |app| {
            if app.active_mesh().is_none() {
                Availability::Unavailable(crate::panels::empty_state())
            } else if app.session.document().objects().len() < 2 {
                Availability::Unavailable(t!("combine.needs_two"))
            } else {
                Availability::Available
            }
        },
        panel: Some(combine_panel),
        on_open: |app| {
            app.clear_selection();
            app.refusal = None;
        },
        ..T::new(
            COMBINE,
            "tool.combine.name",
            icons::COMBINE,
            Category::Shape,
            10,
        )
    })?;
    shell.register_tool(T {
        shortcut: Some(egui::Key::M),
        help: "tool.make_solid.help",
        availability: needs_object,
        panel: Some(solid_panel),
        on_open: |app| app.refusal = None,
        ..T::new(
            MAKE_SOLID,
            "tool.make_solid.name",
            icons::MAKE_SOLID,
            Category::Shape,
            20,
        )
    })?;
    Ok(())
}

// ---------------------------------------------------------------------
// Shared pieces.
// ---------------------------------------------------------------------

fn section(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add_space(theme::SPACE_M);
    let r = ui.label(
        RichText::new(text)
            .size(theme::TYPE_SECTION)
            .color(theme::TEXT_STRONG),
    );
    ui.add_space(2.0);
    r
}

/// The active object's world bounding box.
pub(crate) fn world_bounds(app: &MeshFixerApp, id: ObjectId) -> Option<(DVec3, DVec3)> {
    let o = app.session.document().object(id)?;
    let mesh = o.mesh()?;
    let bb = mesh.bounding_box()?;
    let t = app.shown_transform(id).unwrap_or(o.transform);
    let (lo, hi) = (DVec3::from(bb.min), DVec3::from(bb.max));
    let mut min = DVec3::splat(f64::MAX);
    let mut max = DVec3::splat(f64::MIN);
    for i in 0..8 {
        let c = DVec3::new(
            if i & 1 == 0 { lo.x } else { hi.x },
            if i & 2 == 0 { lo.y } else { hi.y },
            if i & 4 == 0 { lo.z } else { hi.z },
        );
        let w = crate::transform_tool::world_point(&t, c);
        min = min.min(w);
        max = max.max(w);
    }
    Some((min, max))
}

/// Whether the active object is closed, from its analysis (`None` until
/// it's analysed).
fn active_closed(app: &MeshFixerApp) -> Option<bool> {
    app.current_analysis().map(|a| a.diagnostics.is_closed)
}

/// A quick, exact reject: true if `(point, normal)` (widened by the
/// cut-gap slab, as `mesh.split_plane` cuts it) can't possibly cross the
/// world box `(lo, hi)` at all, because every corner is strictly on one
/// side of it. Cheap (8 corners, however big the mesh), so it runs every
/// frame while the gizmo moves; it's a one-sided test (never a false
/// "misses" — a plane that crosses the box may still miss the actual
/// surface inside it, same as `mf_boolean::cut_half`'s own check, which is
/// what actually runs at Apply and has the final say).
fn plane_misses_world_box(point: DVec3, normal: DVec3, gap: f64, lo: DVec3, hi: DVec3) -> bool {
    let Some(n) = normal.try_normalize() else {
        return false;
    };
    let half = gap * 0.5;
    let mut min_d = f64::MAX;
    let mut max_d = f64::MIN;
    for i in 0..8 {
        let c = DVec3::new(
            if i & 1 == 0 { lo.x } else { hi.x },
            if i & 2 == 0 { lo.y } else { hi.y },
            if i & 4 == 0 { lo.z } else { hi.z },
        );
        let d = n.dot(c - point);
        min_d = min_d.min(d);
        max_d = max_d.max(d);
    }
    // Both sides of the gap slab must have a corner for the plane to be
    // able to split the box at all; `max_d < half` or `min_d > -half`
    // alone means every corner is above (or below) the whole slab.
    max_d < half || min_d > -half
}

/// Owner (2026-11-11): the plane's default used to be the world origin,
/// so it silently missed (or grazed) an object placed away from it — the
/// common case for an imported scan. True while the plane, as currently
/// placed, can't cross any of `mesh.split_plane`'s targets at all: shown
/// in the preview before Apply, not only as a refusal after it fails.
fn split_plane_misses_selection(app: &MeshFixerApp) -> bool {
    let selected = app.selected_objects();
    if selected.is_empty() {
        return false;
    }
    let point = app.split.point();
    let normal = app.split.normal();
    let gap = app.split.gap;
    selected.iter().all(|&id| {
        world_bounds(app, id)
            .is_some_and(|(lo, hi)| plane_misses_world_box(point, normal, gap, lo, hi))
    })
}

/// The refusal card: what went wrong in plain words, and the next action
/// as a button where there is one. Returns true if it was shown.
fn refusal_card(app: &mut MeshFixerApp, ui: &mut egui::Ui, op_id: &str) -> bool {
    refusal_card_for(app, ui, op_id)
}

/// As [`refusal_card`], for another tool's panel (Parts, Connectors):
/// shows `app.refusal` when it names `op_id`, with the same plain-
/// language card and next action every tool's panel uses.
pub(crate) fn refusal_card_for(app: &mut MeshFixerApp, ui: &mut egui::Ui, op_id: &str) -> bool {
    let Some(r) = app.refusal.clone().filter(|r| r.op_id == op_id) else {
        return false;
    };
    show_refusal(app, ui, &r.code, &r.message);
    true
}

fn show_refusal(app: &mut MeshFixerApp, ui: &mut egui::Ui, code: &str, message: &str) {
    widgets::card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(icons::WARNING)
                    .font(icons::font(theme::ICON_SIZE))
                    .color(theme::TEXT),
            );
            ui.label(RichText::new(t!("refusal.title")).color(theme::TEXT_STRONG));
        });
        widgets::body(ui, message);
        widgets::caption(ui, format!("({code})"));
        ui.add_space(theme::SPACE_XS);
        let next = next_action(code).map(|n| match n {
            Next::Repair => (
                icons::REPAIR,
                t!("refusal.repair"),
                crate::tool_panels::REPAIR,
            ),
            Next::MakeSolid => (icons::MAKE_SOLID, t!("refusal.make_solid"), MAKE_SOLID),
        });
        if let Some((icon, label, tool)) = next
            && widgets::wide_button(ui, Kind::Secondary, Some(icon), &label).clicked()
        {
            app.refusal = None;
            app.open_rail_tool(tool);
        }
    });
    ui.add_space(theme::SPACE_S);
}

/// The live "the plane misses the model" warning, shown in the preview
/// before Apply can ever refuse it (owner 2026-11-11): the same plain-
/// language, warning-icon card as a refusal, but driven by
/// [`split_plane_misses_selection`] every frame the plane moves, not by a
/// run that already failed.
pub(crate) fn plane_miss_warning(app: &MeshFixerApp, ui: &mut egui::Ui) {
    if !split_plane_misses_selection(app) {
        return;
    }
    let doc = app.session.document();
    let name = app
        .selected_objects()
        .first()
        .and_then(|&id| doc.object(id))
        .map_or_else(|| t!("split.the_model"), |o| o.name.clone());
    let message = t!("split.misses_warning", name = name);
    widgets::card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(icons::WARNING)
                    .font(icons::font(theme::ICON_SIZE))
                    .color(theme::TEXT),
            );
            ui.label(RichText::new(t!("refusal.title")).color(theme::TEXT_STRONG));
        });
        widgets::body(ui, &message);
    });
    ui.add_space(theme::SPACE_S);
}

enum Next {
    Repair,
    MakeSolid,
}

/// The next action a refusal code points to, as a button.
fn next_action(code: &str) -> Option<Next> {
    match code {
        "operand_not_closed"
        | "tool_not_closed"
        | "opening_not_covered"
        | "opening_too_large_for_join" => Some(Next::Repair),
        "operand_overlaps_itself" | "section_self_intersects" | "result_self_intersects" => {
            Some(Next::MakeSolid)
        }
        _ => None,
    }
}

/// Run `op_id` on the active object with `params` (and `operands`), on
/// the job pool (progress and Cancel in the status bar).
fn run(
    app: &mut MeshFixerApp,
    op_id: &str,
    label: String,
    params: BTreeMap<String, String>,
    operands: Vec<(String, ObjectId, Option<mf_selection::Selection>)>,
) {
    let Some(target) = app.active else { return };
    run_on(app, target, op_id, label, params, operands);
}

/// As [`run`], on `target` rather than the active object.
fn run_on(
    app: &mut MeshFixerApp,
    target: ObjectId,
    op_id: &str,
    label: String,
    params: BTreeMap<String, String>,
    operands: Vec<(String, ObjectId, Option<mf_selection::Selection>)>,
) {
    app.refusal = None;
    app.run_invocation(
        Invocation {
            op_id: op_id.into(),
            target,
            selection: None,
            operands,
            raw_params: params,
        },
        label,
    );
}

// ---------------------------------------------------------------------
// Split.
// ---------------------------------------------------------------------

/// Place the plane through the world bounding-box centre of every target
/// `mesh.split_plane` would cut ([`MeshFixerApp::selected_objects`]:
/// usually just the active object, but every object picked together when
/// there's more than one), horizontal (when the selection differs from
/// last time, or `force`).
///
/// Owner (2026-11-11): the plane used to default through the world
/// origin, so an object placed away from it (the common case: an
/// imported scan) was missed or grazed by the default cut — "Split
/// looked broken". Centring it on the selection's own bounds instead
/// means Apply with the defaults always crosses the model.
pub(crate) fn place_plane(app: &mut MeshFixerApp, force: bool) {
    let selected = app.selected_objects();
    if selected.is_empty() {
        return;
    }
    if !force && app.split.placed_for.as_deref() == Some(selected.as_slice()) {
        return;
    }
    let mut bounds: Option<(DVec3, DVec3)> = None;
    for &id in &selected {
        let Some((lo, hi)) = world_bounds(app, id) else {
            continue;
        };
        bounds = Some(match bounds {
            Some((blo, bhi)) => (blo.min(lo), bhi.max(hi)),
            None => (lo, hi),
        });
    }
    if let Some((lo, hi)) = bounds {
        app.split.set((lo + hi) * 0.5, (0.0, 0.0));
        app.split.placed_for = Some(selected);
    }
}

/// The plane's gizmo (move or tilt), at the plane's point, while Split is
/// open; with its highlighted handle.
pub fn split_gizmo(app: &MeshFixerApp) -> Option<(Gizmo, Option<Handle>)> {
    if app.active_tool != SPLIT || app.active_mesh().is_none() || app.busy.is_some() {
        return None;
    }
    // Print volume: the box's own gizmo.
    if app.split_flow.mode == crate::split_flow::CutMode::Volume {
        return crate::split_flow::box_gizmo(app);
    }
    let s = &app.split;
    let g = Gizmo {
        pivot: s.point(),
        mode: s.mode,
        size_pt: crate::transform_tool::GIZMO_SIZE_PT,
    };
    // Move: only the arrow along the normal matters most, but every
    // handle works (the plane is a placement).
    let highlight = s.drag.map(|(d, _)| d.handle()).or(s.hover);
    Some((g, highlight))
}

/// How the view is shaded: the section view while Split shows it, else
/// the View menu's choice.
pub fn shading(app: &MeshFixerApp) -> mf_render::Shading {
    if split_preview(app).is_some() && app.split.section {
        mf_render::Shading {
            preset: mf_render::ShadingPreset::XRay,
            ..app.shading
        }
    } else {
        app.shading
    }
}

/// The plane as the renderer's split preview: (point, normal) in world.
pub fn split_preview(app: &MeshFixerApp) -> Option<(DVec3, DVec3)> {
    (app.active_tool == SPLIT
        && app.active_mesh().is_some()
        && app.split_flow.mode == crate::split_flow::CutMode::Slice)
        .then(|| (app.split.point(), app.split.normal()))
}

pub(crate) fn plane_input(app: &mut MeshFixerApp, input: &crate::shell::ToolInput) -> Input {
    use crate::viewport::ToolPointer;
    let Some((g, _)) = split_gizmo(app) else {
        return Input::Pass;
    };
    let (Some(cam), Some(rect)) = (app.camera, app.viewport.last_rect) else {
        return Input::Pass;
    };
    let view = (rect.width() as f64, rect.height() as f64);
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
    match input.pointer {
        Some(ToolPointer::Start { from, at }) => {
            if let Some(h) = g.hit(&cam, view, from) {
                let start = app.split.plane;
                let drag = g.begin(h, from, &cam, view, start);
                app.split.plane = drag.update(at, &snap);
                app.split.drag = Some((drag, start));
            }
        }
        Some(ToolPointer::Move(px)) | Some(ToolPointer::End(px)) => {
            if let Some((drag, _)) = app.split.drag {
                app.split.plane = drag.update(px, &snap);
            }
            if matches!(input.pointer, Some(ToolPointer::End(_))) {
                app.split.drag = None;
            }
        }
        Some(ToolPointer::Click(px)) => {
            // Off the gizmo, on a target's own surface: read it as
            // "keep this side" (owner 2026-11-12), the side classified
            // the same way the preview shades it (which side of the
            // plane the clicked point is on).
            if g.hit(&cam, view, px).is_none()
                && let Some(world) = surface_click_point(app, &cam, view, px)
            {
                let point = app.split.point();
                let normal = app.split.normal();
                app.split.keep = if normal.dot(world - point) > 0.0 {
                    "above".into()
                } else {
                    "below".into()
                };
            }
        }
        None => {}
    }
    app.split.hover = if app.split.drag.is_some() {
        None
    } else {
        input.hover.and_then(|px| g.hit(&cam, view, px))
    };
    Input::Consumed
}

/// The world-space point where a screen click lands on the nearest
/// selected target's surface, if any (Split's "click a side to keep
/// it").
fn surface_click_point(
    app: &MeshFixerApp,
    cam: &mf_render::OrbitCamera,
    view: (f64, f64),
    px: (f64, f64),
) -> Option<DVec3> {
    let (origin, dir) = cam.ray(cam.target, view, px);
    let (origin, dir) = (DVec3::from(origin), DVec3::from(dir));
    app.selected_objects()
        .into_iter()
        .filter_map(|id| {
            let bvh = app.picking_bvh(id)?;
            let mesh = app.session.document().object(id)?.mesh()?;
            let transform = app.shown_transform(id).unwrap_or_else(|| {
                app.session
                    .document()
                    .object(id)
                    .map(|o| o.transform)
                    // allow-swallow: id just came from selected_objects, which only names live objects; a vanished one falls back to identity rather than panicking this frame.
                    .unwrap_or_default()
            });
            let model = mf_render::model_matrix(&transform);
            let (_, t) = bvh.ray_pick(mesh, origin.to_array(), dir.to_array(), &model)?;
            Some(t)
        })
        .min_by(f64::total_cmp)
        .map(|t| origin + dir * t)
}

/// An open model can't be cut (the operation refuses it by name): say so
/// up front, with the way on (Repair). True if it's open.
pub(crate) fn open_model_card(app: &mut MeshFixerApp, ui: &mut egui::Ui) -> bool {
    let open = active_closed(app) == Some(false);
    if open {
        show_refusal(app, ui, "operand_not_closed", &t!("split.refuse_open"));
    }
    open
}

/// Step 1's Slice controls: the plane (presets, move or tilt, typed point
/// and tilt), the section view, which parts to keep and the gap.
pub(crate) fn slice_controls(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    place_plane(app, false);
    let units = crate::defects::unit_symbol(app.session.document().units());
    // Before Apply can ever refuse it: the plane misses or grazes
    // (owner 2026-11-11, "Also"). `plane_miss_warning` re-checks every
    // frame the gizmo moves, so dragging the plane into the model clears
    // it live, not only after a failed Apply.
    plane_miss_warning(app, ui);
    section(ui, &t!("split.section.plane"));
    widgets::caption(ui, t!("split.plane_help"));
    ui.add_space(theme::SPACE_XS);
    // Presets.
    ui.horizontal(|ui| {
        let w = (ui.available_width() - 2.0 * ui.spacing().item_spacing.x) / 3.0;
        let point = app.split.point();
        for (label, tilt) in [
            (t!("split.horizontal"), (0.0, 0.0)),
            (t!("split.vertical_x"), (0.0, 90.0)),
            (t!("split.vertical_y"), (-90.0, 0.0)),
        ] {
            if widgets::button(ui, Kind::Secondary, None, &label, w).clicked() {
                app.split.set(point, tilt);
            }
        }
    });
    ui.add_space(theme::SPACE_XS);
    let current = match app.split.mode {
        GizmoMode::Rotate => 1,
        _ => 0,
    };
    let modes = [
        (t!("split.move"), t!("split.move")),
        (t!("split.tilt"), t!("split.tilt")),
    ];
    let options: Vec<(&str, &str, bool)> = modes
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str(), true))
        .collect();
    let (clicked, _) = widgets::segmented(ui, &options, current);
    if let Some(i) = clicked {
        app.split.mode = if i == 0 {
            GizmoMode::Move
        } else {
            GizmoMode::Rotate
        };
        app.split.drag = None;
    }
    // Typed point and tilt.
    section(ui, &t!("split.section.point", unit = units));
    let mut p = app.split.point();
    let mut changed = false;
    ui.horizontal(|ui| {
        for (axis, v) in [(0usize, &mut p.x), (1, &mut p.y), (2, &mut p.z)] {
            let color = [theme::AXIS_X, theme::AXIS_Y, theme::AXIS_Z][axis];
            ui.label(
                RichText::new(crate::transform_tool::AXIS_NAMES[axis])
                    .size(theme::TYPE_CAPTION)
                    .color(color),
            );
            let r = ui.add(egui::DragValue::new(v).speed(0.5).max_decimals(2));
            let name = format!("Split point {}", crate::transform_tool::AXIS_NAMES[axis]);
            r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::SpinButton, true, &name));
            changed |= r.changed();
        }
    });
    section(ui, &t!("split.section.tilt"));
    let (mut rx, mut ry) = app.split.tilt();
    ui.horizontal(|ui| {
        for (axis, v) in [(0usize, &mut rx), (1, &mut ry)] {
            let color = [theme::AXIS_X, theme::AXIS_Y][axis];
            ui.label(
                RichText::new(crate::transform_tool::AXIS_NAMES[axis])
                    .size(theme::TYPE_CAPTION)
                    .color(color),
            );
            let r = ui.add(
                egui::DragValue::new(v)
                    .speed(1.0)
                    .range(-180.0..=180.0)
                    .suffix("°")
                    .max_decimals(1),
            );
            let name = format!("Tilt about {}", crate::transform_tool::AXIS_NAMES[axis]);
            r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::SpinButton, true, &name));
            changed |= r.changed();
        }
    });
    if changed {
        app.split.set(p, (rx, ry));
    }
    ui.add_space(theme::SPACE_XS);
    ui.checkbox(&mut app.split.section, t!("split.section_view"))
        .on_hover_text(t!("split.section_view.tip"));
    // Settings.
    section(ui, &t!("split.section.keep"));
    // "Above"/"below" means nothing for a vertical or tilted plane (owner
    // 2026-11-12): the two Keep buttons are named, and coloured, by the
    // plane's actual orientation; "both" always stays plain. The engine's
    // `keep` values ("both"/"above"/"below") don't change — "above" is
    // still side A (the normal's side), "below" still side B.
    let sides = app.split.side_names();
    let (label_a, label_b) = side_name_labels(sides);
    let keeps = [
        ("both", t!("split.keep.both"), None),
        (
            "above",
            crate::i18n::tr(label_a, &[]),
            Some(theme::SPLIT_SIDE_A),
        ),
        (
            "below",
            crate::i18n::tr(label_b, &[]),
            Some(theme::SPLIT_SIDE_B),
        ),
    ];
    let options: Vec<(&str, &str, bool, Option<egui::Color32>)> = keeps
        .iter()
        .map(|(_, l, c)| (l.as_str(), l.as_str(), true, *c))
        .collect();
    let current = keeps
        .iter()
        .position(|(k, _, _)| *k == app.split.keep)
        .expect("the keep choice is listed");
    if let (Some(i), _) = widgets::segmented_colored(ui, &options, current) {
        app.split.keep = keeps[i].0.into();
    }
    section(ui, &t!("split.section.gap", unit = units));
    ui.add(
        egui::Slider::new(&mut app.split.gap, 0.0..=5.0)
            .suffix(format!(" {units}"))
            .max_decimals(2),
    )
    .on_hover_text(t!("split.gap_help"));
}

// ---------------------------------------------------------------------
// Combine.
// ---------------------------------------------------------------------

/// The selected objects split by role: (solids, holes), each in pick
/// order.
pub fn combine_roles(app: &MeshFixerApp) -> (Vec<ObjectId>, Vec<ObjectId>) {
    app.selected_objects()
        .into_iter()
        .partition(|&id| !app.is_hole(id))
}

/// "Joins N solids and cuts M holes", in whole sentences per plural.
fn combine_summary(solids: usize, holes: usize) -> String {
    match (solids == 1, holes == 1) {
        (true, true) => t!("combine.summary.one_one", s = solids, h = holes),
        (true, false) => t!("combine.summary.one_other", s = solids, h = holes),
        (false, true) => t!("combine.summary.other_one", s = solids, h = holes),
        (false, false) => t!("combine.summary.other_other", s = solids, h = holes),
    }
}

fn combine_panel(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    refusal_card(app, ui, "mesh.combine");
    refusal_card(app, ui, "mesh.boolean");
    let selected = app.selected_objects();
    let (solids, holes) = combine_roles(app);
    if selected.len() < 2 {
        widgets::body(ui, t!("combine.select_more"));
    } else {
        widgets::body(ui, combine_summary(solids.len(), holes.len()));
    }
    // Each selected object with its role, and a toggle to switch it (H,
    // and the Browser's menu, do the same in one undo step).
    section(ui, &t!("combine.section.objects"));
    let doc = app.session.document();
    let rows: Vec<(ObjectId, String, bool)> = selected
        .iter()
        .filter_map(|&id| doc.object(id).map(|o| (id, o.name.clone(), o.is_hole())))
        .collect();
    for (id, name, hole) in rows {
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let action = if hole {
                    t!("role.make_solid")
                } else {
                    t!("role.make_hole")
                };
                if widgets::icon_button(
                    ui,
                    if hole {
                        icons::OBJECT
                    } else {
                        icons::HOLE_ROLE
                    },
                    &format!("{name}: {action}"),
                    None,
                    22.0,
                )
                .on_hover_text(format!("{action} (H)"))
                .clicked()
                {
                    app.toggle_role(&[id]);
                }
                ui.label(
                    RichText::new(if hole {
                        icons::HOLE_ROLE
                    } else {
                        icons::OBJECT
                    })
                    .font(icons::font(theme::ICON_SIZE - 2.0))
                    .color(theme::TEXT_MUTED),
                );
                let role = if hole {
                    t!("combine.role.hole")
                } else {
                    t!("combine.role.solid")
                };
                widgets::body(ui, format!("{name} · {role}"));
            });
        });
    }
    widgets::caption(ui, t!("combine.role_help"));
    if !selected.is_empty() && solids.is_empty() {
        widgets::caption(ui, t!("combine.needs_solid"));
    }
    ui.add_space(theme::SPACE_L);
    let can = app.busy.is_none() && selected.len() >= 2 && !solids.is_empty();
    if ui
        .add_enabled_ui(can, |ui| {
            widgets::wide_button(
                ui,
                Kind::Primary,
                Some(icons::COMBINE),
                &t!("combine.apply"),
            )
        })
        .inner
        .clicked()
        && let Some((&first, others)) = solids.split_first()
    {
        let operands = others
            .iter()
            .map(|&id| ("solids".to_owned(), id, None))
            .chain(holes.iter().map(|&id| ("holes".to_owned(), id, None)))
            .collect();
        let params =
            BTreeMap::from([("keep_holes".to_owned(), app.combine.keep_holes.to_string())]);
        run_on(
            app,
            first,
            "mesh.combine",
            t!("combine.apply"),
            params,
            operands,
        );
    }
    ui.add_space(theme::SPACE_S);
    egui::CollapsingHeader::new(t!("combine.more"))
        .id_salt("combine more")
        .show(ui, |ui| {
            ui.checkbox(&mut app.combine.keep_holes, t!("combine.keep_holes"));
            ui.add_space(theme::SPACE_XS);
            // Intersect: the overlap of exactly two objects.
            let two = selected.len() == 2;
            let can = app.busy.is_none() && two;
            if ui
                .add_enabled_ui(can, |ui| {
                    widgets::wide_button(ui, Kind::Secondary, None, &t!("combine.intersect"))
                })
                .inner
                .clicked()
            {
                let params = BTreeMap::from([("operation".to_owned(), "intersect".to_owned())]);
                run_on(
                    app,
                    selected[0],
                    "mesh.boolean",
                    t!("combine.intersect"),
                    params,
                    vec![("tool".to_owned(), selected[1], None)],
                );
            }
            widgets::caption(
                ui,
                if two {
                    t!("combine.intersect_help")
                } else {
                    t!("combine.intersect_needs_two")
                },
            );
        });
}

// ---------------------------------------------------------------------
// Make solid.
// ---------------------------------------------------------------------

fn solid_panel(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    refusal_card(app, ui, "mesh.make_solid");
    let units = crate::defects::unit_symbol(app.session.document().units());
    let Some(id) = app.active else { return };
    let Some((lo, hi)) = world_bounds(app, id) else {
        widgets::caption(ui, "Measuring the object…");
        return;
    };
    let diag = (hi - lo).length().max(1e-9);
    // As the operation chooses by default: 1/256 of the diagonal.
    let auto = diag / 256.0;
    if app.solid.for_object != Some(id) || app.solid.cell.is_none() {
        app.solid = SolidState {
            cell: Some(auto),
            offset: 0.0,
            smooth: false,
            for_object: Some(id),
        };
    }
    widgets::body(ui, t!("solid.what"));
    section(ui, &t!("solid.section.cell", unit = units));
    let mut cell = app.solid.cell.unwrap_or(auto);
    ui.scope(|ui| {
        ui.spacing_mut().slider_width = (ui.available_width() - 90.0).max(60.0);
        let r = ui.add(
            egui::Slider::new(&mut cell, diag / 1024.0..=diag / 32.0)
                .logarithmic(true)
                .suffix(format!(" {units}"))
                .max_decimals(3),
        );
        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Slider, true, "Cell size"));
    });
    app.solid.cell = Some(cell);
    // The trade-off, in plain words, with the size it implies.
    let dims = (hi - lo) / cell;
    let points = (dims.x.max(1.0) + 3.0) * (dims.y.max(1.0) + 3.0) * (dims.z.max(1.0) + 3.0);
    widgets::caption(
        ui,
        t!(
            "solid.tradeoff",
            cell = crate::defects::measure(cell),
            unit = units
        ),
    );
    if points > 40_000_000.0 {
        widgets::caption(ui, t!("solid.too_fine"));
    }
    section(ui, &t!("solid.section.offset", unit = units));
    ui.add(
        egui::DragValue::new(&mut app.solid.offset)
            .speed(0.05)
            .suffix(format!(" {units}"))
            .max_decimals(2),
    )
    .on_hover_text(t!("solid.offset_help"));
    ui.add_space(theme::SPACE_XS);
    ui.checkbox(&mut app.solid.smooth, t!("solid.smooth"))
        .on_hover_text(t!("solid.smooth_help"));
    ui.add_space(theme::SPACE_L);
    if ui
        .add_enabled_ui(app.busy.is_none(), |ui| {
            widgets::wide_button(
                ui,
                Kind::Primary,
                Some(icons::MAKE_SOLID),
                &t!("solid.apply"),
            )
        })
        .inner
        .clicked()
    {
        let params = BTreeMap::from([
            ("cell_size".to_owned(), cell.to_string()),
            ("offset".to_owned(), app.solid.offset.to_string()),
            ("preserve_sharp".to_owned(), (!app.solid.smooth).to_string()),
        ]);
        run(
            app,
            "mesh.make_solid",
            t!("solid.apply"),
            params,
            Vec::new(),
        );
    }
}

// ---------------------------------------------------------------------
// What the Shape tools draw over the view.
// ---------------------------------------------------------------------

/// Split's plane as a translucent disc over the 3D view.
pub fn view_overlay(app: &MeshFixerApp, ctx: &egui::Context) {
    let (Some(rect), Some(cam)) = (app.viewport.last_rect, app.camera) else {
        return;
    };
    let view = (rect.width() as f64, rect.height() as f64);
    let at = |p: DVec3| {
        mf_render::screen_position(&cam, cam.target, view, p.to_array())
            .map(|(x, y)| rect.left_top() + egui::vec2(x as f32, y as f32))
    };
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Middle,
        egui::Id::unique("shape tools overlay"),
    ));
    let painter = painter.with_clip_rect(rect);
    if let (Some((point, normal)), Some(id)) = (split_preview(app), app.active)
        && let Some((lo, hi)) = world_bounds(app, id)
    {
        let r = (hi - lo).length() * 0.6;
        let u = normal.any_orthonormal_vector();
        let v = normal.cross(u);
        let pts: Vec<egui::Pos2> = (0..64)
            .filter_map(|i| {
                let a = std::f64::consts::TAU * i as f64 / 64.0;
                at(point + (u * a.cos() + v * a.sin()) * r)
            })
            .collect();
        if pts.len() == 64 {
            painter.add(egui::Shape::convex_polygon(
                pts.clone(),
                theme::HOVER.gamma_multiply(0.07),
                egui::Stroke::NONE,
            ));
            painter.add(egui::Shape::closed_line(
                pts,
                egui::Stroke::new(1.0, theme::HOVER.gamma_multiply(0.55)),
            ));
        }
    }
    crate::split_flow::overlay(app, &painter, rect);
    crate::connect_tool::overlay(app, &painter, rect);
    crate::hinge_tool::overlay(app, &painter, rect);
}
