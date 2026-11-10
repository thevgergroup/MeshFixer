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
use crate::shell::{Availability, Category, Claims, Input, RegisterError, Shell, Tool as T};
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
    /// The object the plane was placed for (a new active object places it
    /// again).
    pub placed_for: Option<ObjectId>,
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
}

/// Combine's settings: whether the holes are kept (Combine acts on the
/// objects selected together, [`MeshFixerApp::selected_objects`], each by
/// its Solid/Hole role).
#[derive(Debug, Default)]
pub struct CombineState {
    pub keep_holes: bool,
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
        panel: Some(split_panel),
        claims: |app| {
            split_gizmo(app).and_then(|(g, _)| {
                let cam = app.camera?;
                let rect = app.viewport.last_rect?;
                let view = (rect.width() as f64, rect.height() as f64);
                Some(Box::new(move |px: (f64, f64)| g.hit(&cam, view, px).is_some()) as Claims)
            })
        },
        viewport_input: split_input,
        on_open: |app| {
            app.clear_selection();
            app.refusal = None;
            place_plane(app, false);
        },
        on_close: |app, _| {
            app.split.drag = None;
            app.split.hover = None;
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
fn world_bounds(app: &MeshFixerApp, id: ObjectId) -> Option<(DVec3, DVec3)> {
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

/// The refusal card: what went wrong in plain words, and the next action
/// as a button where there is one. Returns true if it was shown.
fn refusal_card(app: &mut MeshFixerApp, ui: &mut egui::Ui, op_id: &str) -> bool {
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

fn vec3(v: DVec3) -> String {
    format!("{},{},{}", v.x, v.y, v.z)
}

// ---------------------------------------------------------------------
// Split.
// ---------------------------------------------------------------------

/// Place the plane through the active object's centre, horizontal (when
/// Split opens on another object than last time, or `force`).
fn place_plane(app: &mut MeshFixerApp, force: bool) {
    let Some(id) = app.active else { return };
    if !force && app.split.placed_for == Some(id) {
        return;
    }
    if let Some((lo, hi)) = world_bounds(app, id) {
        app.split.set((lo + hi) * 0.5, (0.0, 0.0));
        app.split.placed_for = Some(id);
    }
}

/// The plane's gizmo (move or tilt), at the plane's point, while Split is
/// open; with its highlighted handle.
pub fn split_gizmo(app: &MeshFixerApp) -> Option<(Gizmo, Option<Handle>)> {
    if app.active_tool != SPLIT || app.active_mesh().is_none() || app.busy.is_some() {
        return None;
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
    (app.active_tool == SPLIT && app.active_mesh().is_some())
        .then(|| (app.split.point(), app.split.normal()))
}

fn split_input(app: &mut MeshFixerApp, input: &crate::shell::ToolInput) -> Input {
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
        Some(ToolPointer::Click(_)) | None => {}
    }
    app.split.hover = if app.split.drag.is_some() {
        None
    } else {
        input.hover.and_then(|px| g.hit(&cam, view, px))
    };
    Input::Consumed
}

fn split_panel(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    place_plane(app, false);
    let units = crate::defects::unit_symbol(app.session.document().units());
    // An open model can't be cut (the operation refuses it by name); say
    // so up front, with the way on.
    let open = active_closed(app) == Some(false);
    if open {
        show_refusal(app, ui, "operand_not_closed", &t!("split.refuse_open"));
    } else {
        refusal_card(app, ui, "mesh.split_plane");
    }
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
    let keeps = [
        ("both", t!("split.keep.both")),
        ("above", t!("split.keep.above")),
        ("below", t!("split.keep.below")),
    ];
    let options: Vec<(&str, &str, bool)> = keeps
        .iter()
        .map(|(_, l)| (l.as_str(), l.as_str(), true))
        .collect();
    let current = keeps
        .iter()
        .position(|(k, _)| *k == app.split.keep)
        .expect("the keep choice is listed");
    if let (Some(i), _) = widgets::segmented(ui, &options, current) {
        app.split.keep = keeps[i].0.into();
    }
    section(ui, &t!("split.section.gap", unit = units));
    ui.add(
        egui::Slider::new(&mut app.split.gap, 0.0..=5.0)
            .suffix(format!(" {units}"))
            .max_decimals(2),
    )
    .on_hover_text(t!("split.gap_help"));
    widgets::caption(ui, t!("split.result"));
    let n = app.selected_objects().len();
    if n > 1 {
        widgets::caption(ui, crate::tp!("split.targets", n));
    }
    ui.add_space(theme::SPACE_L);
    let can = app.busy.is_none() && !open;
    if ui
        .add_enabled_ui(can, |ui| {
            widgets::wide_button(ui, Kind::Primary, Some(icons::SPLIT), &t!("split.apply"))
        })
        .inner
        .clicked()
    {
        let s = &app.split;
        let params = BTreeMap::from([
            ("plane_point".to_owned(), vec3(s.point())),
            ("plane_normal".to_owned(), vec3(s.normal())),
            ("keep".to_owned(), s.keep.clone()),
            ("cut_gap".to_owned(), s.gap.to_string()),
        ]);
        // Every selected object, in one command: the first is the target,
        // the rest more targets.
        let selected = app.selected_objects();
        let Some((&first, rest)) = selected.split_first() else {
            return;
        };
        let more = rest
            .iter()
            .map(|&id| (mf_command::TARGET_OPERAND.to_owned(), id, None))
            .collect();
        run_on(
            app,
            first,
            "mesh.split_plane",
            t!("split.apply"),
            params,
            more,
        );
    }
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
}
