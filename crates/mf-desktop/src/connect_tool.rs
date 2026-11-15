//! The Connect tool (owner 2026-11-11): add connectors to a cut joint,
//! registered through `shell::Tool` like every other tool, in the Shape
//! group next to Split. Split only cuts; this is the whole of what used
//! to be the three-step Split workflow's step 3, on its own rail entry
//! with its own icon.
//!
//! - **Joints.** Lists the document's joints (`app.split_flow.joints`,
//!   made by Split's cuts). Clicking a cut face in the view picks its
//!   joint, same as clicking its row.
//! - **Once a joint is picked:** choose which piece is transparent
//!   (drawn see-through, the other piece's cut face solid and facing
//!   the view); click on that face to place a connector of the chosen
//!   kind (the registry's `joint.*` connectors); drag, resize (a handle
//!   on its rim), set its depth (the square handle) or delete it
//!   (Delete). Its counterpart on the transparent side draws live, from
//!   the very parts `split.place_connectors` will build. Live checks
//!   (breakout, edge distance, dowel length, fit) show per connector in
//!   plain words, each with a one-click fix.
//! - **Apply** runs `split.place_connectors` once for every connector
//!   on the joint; a joint can be reopened and applied again (from the
//!   revision before its connectors).
//! - **Add as editable shapes** is Connect as a placement helper
//!   (parametric-shapes brief): each connector becomes a linked group of
//!   editable shapes, aligned across the cut (a solid peg on the piece
//!   it was placed on, its hole on the other, each marked with the piece
//!   it joins), so it can still be resized, moved as one, and attached
//!   later (`object.attach`). The same as placing them by hand.
//! - **With no joints**, the panel says so and offers a button that
//!   opens Split.
//!
//! The joint bookkeeping, background geometry read, checks and previews
//! are shared state on `app.split_flow` (`crate::split_flow`), since a
//! joint outlives the Split tool that made it.

use std::collections::BTreeMap;

use eframe::egui::{self, RichText};
use glam::DVec3;
use mf_operations::{CapRegion, Check, JointSide, Placement};
use mf_units::{Length, Unit};

use crate::MeshFixerApp;
use crate::intent::Intent;
use crate::shell::{Availability, Category, Input, RegisterError, Shell, Tool as T};
use crate::split_flow::{self, Drag, GeomSlot, Hit, Joint, applied_now, exists, name_of};
use crate::widgets::{self, Kind, LengthFieldOpts, length_field};
use crate::{icons, t, theme};

pub const CONNECT: &str = "connect";

/// Document units per millimetre (the inverse of
/// [`mf_units::Unit::mm_per`]): the scale every `mf_operations::split`
/// geometry helper here needs, since the connector generators they call
/// are mm-internal (D16) while a joint's own frame and placements are
/// recorded in document units. This is never a panel conversion (ADR-0006
/// rule 2: a field holds a typed `Length`, and only `Session::prepare`
/// converts it); it feeds `placement_params`, `placement_to_world`,
/// `footprint_2d` and `joint_checks`, none of which read or write a
/// widget.
fn doc_units_per_millimetre(app: &MeshFixerApp) -> f64 {
    1.0 / Length::new(1.0, app.session.document().units()).mm()
}

/// A placement's turn on the cut face, degrees: a full turn either way.
/// Named (not a literal range on the widget itself) since the range
/// isn't a length: the lint's literal-range rule exists for lengths that
/// should go through `length_field`, not for this angle.
const ANGLE_RANGE: std::ops::RangeInclusive<f64> = -180.0..=180.0;

/// Delete and Backspace, while Connect is open: both remove the picked
/// connector, never the active object (owner 2026-11-11).
const DELETE_KEYS: &[(egui::Key, crate::tools::ToolKey)] = &[
    (egui::Key::Delete, crate::tools::ToolKey::DeleteSelection),
    (egui::Key::Backspace, crate::tools::ToolKey::DeleteSelection),
];

pub fn register(shell: &mut Shell) -> Result<(), RegisterError> {
    shell.register_tool(T {
        help: "tool.connect.help",
        availability: |_| Availability::Available,
        panel: Some(panel),
        claims,
        viewport_input: input,
        keys: |_| DELETE_KEYS,
        on_key: |app, key| {
            if key == crate::tools::ToolKey::DeleteSelection {
                split_flow::delete_selected(app);
            }
        },
        on_open: |app| {
            app.refusal = None;
            if app.split_flow.joint.is_none() {
                let first = app.split_flow.joints.iter().rposition(|j| exists(app, j));
                if let Some(j) = first {
                    split_flow::open_joint(app, j);
                }
            }
            app.split_flow.framed = None;
        },
        on_close: |app, _| {
            app.split_flow.drag = None;
            app.split_flow.hover = None;
        },
        ..T::new(
            CONNECT,
            "tool.connect.name",
            icons::CONNECT,
            Category::Shape,
            1,
        )
    })
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

/// True if `p` cuts a round hole that prints sideways on this joint
/// (its axis within 45° of horizontal) but is kept round, so its top
/// would sag.
pub fn sags_sideways(app: &MeshFixerApp, joint: &Joint, p: &Placement) -> bool {
    let mm = doc_units_per_millimetre(app);
    let Ok(params) = mf_operations::placement_params(p, joint.frame.gap / mm) else {
        return false;
    };
    let world = mf_operations::placement_to_world(&joint.frame, p, mm);
    mf_parts::shape::params_sag_sideways(&params, glam::DMat3::from_mat4(world))
}

/// A new placement's own params: every param its schema ties to the fit
/// (`fit_role`) resolved from Connect's Fit class and the Machine
/// profile (rounded to a micron), recorded as numbers like every
/// placement param.
fn new_params(app: &MeshFixerApp, kind: &str) -> BTreeMap<String, f64> {
    let Some(g) = mf_parts::find_generator(kind) else {
        return BTreeMap::new();
    };
    let s = split_flow::settings(app);
    let mut raw = BTreeMap::new();
    mf_parts::fill_fit_role_params(g.spec(), &mut raw, &s.fit_class, &s.machine);
    raw.into_iter()
        .filter_map(|(k, v)| Some((k, (v.parse::<f64>().ok()? * 1000.0).round() / 1000.0))) // allow-swallow: a fit-role value that isn't a number (a class's name) isn't a placement number
        .collect()
}

/// Add a connector of the chosen kind at `at` on the open joint's solid
/// cut face; it becomes the picked one. A kind placed as a line
/// (`PlacementHint::DragLine`) placed with a click is a line of no length
/// along the joint's own u.
pub fn add_connector(app: &mut MeshFixerApp, at: [f64; 2]) {
    let kind = app.split_flow.kind();
    if mf_parts::find_generator(kind)
        .is_some_and(|g| g.placement_hint() == mf_parts::PlacementHint::DragLine)
    {
        return add_line(app, at, at);
    }
    let params = new_params(app, kind);
    push_placement(app, kind, at, 0.0, params);
}

/// Add a connector of the chosen kind dragged as a line from `start` to
/// `end` on the open joint's solid cut face (`PlacementHint::DragLine`,
/// D23): its centre is the line's middle and its turn the line's
/// direction; its generator works out the rest from half the dragged
/// length and the cut faces (`Generator::line_params`, measuring them
/// through `mf_operations::CapTargets`).
pub fn add_line(app: &mut MeshFixerApp, start: [f64; 2], end: [f64; 2]) {
    let kind = app.split_flow.kind();
    let Some(g) = mf_parts::find_generator(kind) else {
        return;
    };
    let (dx, dy) = (end[0] - start[0], end[1] - start[1]);
    let len = (dx * dx + dy * dy).sqrt();
    let dir = if len > 1e-6 {
        [dx / len, dy / len]
    } else {
        [1.0, 0.0]
    };
    let scale = doc_units_per_millimetre(app);
    let Some(on) = app.split_flow.open_joint().map(|j| j.transparent.other()) else {
        return;
    };
    let p = Placement {
        connector: kind.to_owned(),
        on,
        at: [(start[0] + end[0]) / 2.0, (start[1] + end[1]) / 2.0],
        angle: dir[1].atan2(dir[0]).to_degrees(),
        params: new_params(app, kind),
    };
    let patch = match app.split_flow.geometry() {
        Some(geom) => {
            let targets = mf_operations::CapTargets {
                caps: [geom.cap(JointSide::A), geom.cap(JointSide::B)],
                placement: &p,
                mm: scale,
            };
            let run = mf_runtime::RunContext::none();
            g.line_params(
                len / 2.0 / scale,
                &mf_parts::GenContext::with_targets(&run, &targets),
            )
        }
        None => g.line_params(
            len / 2.0 / scale,
            &mf_parts::GenContext::new(&mf_runtime::RunContext::none()),
        ),
    };
    let p = mf_operations::patched(&p, &patch);
    push_placement(app, kind, p.at, p.angle, p.params);
}

fn push_placement(
    app: &mut MeshFixerApp,
    kind: &str,
    at: [f64; 2],
    angle: f64,
    params: BTreeMap<String, f64>,
) {
    let f = &mut app.split_flow;
    let Some(joint) = f.joint.and_then(|j| f.joints.get_mut(j)) else {
        return;
    };
    joint.placements.push(Placement {
        connector: kind.to_owned(),
        on: joint.transparent.other(),
        at,
        angle,
        params,
    });
    f.selected = Some(joint.placements.len() - 1);
}

fn panel(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    split_flow::refresh(app);
    crate::shape_tools::refusal_card_for(app, ui, "split.place_connectors");
    crate::shape_tools::refusal_card_for(app, ui, "object.add_shape");
    // The joints.
    section(ui, &t!("connect.section.joints"));
    let rows: Vec<(usize, String, String)> = app
        .split_flow
        .joints
        .iter()
        .enumerate()
        .filter(|(_, j)| exists(app, j))
        .map(|(i, j)| {
            let status = if applied_now(app, j) {
                crate::tp!("connect.joint.applied", j.placements.len())
            } else if j.placements.is_empty() {
                t!("connect.joint.empty")
            } else {
                crate::tp!("connect.joint.draft", j.placements.len())
            };
            (
                i,
                t!(
                    "connect.joint.row",
                    a = name_of(app, j.a),
                    b = name_of(app, j.b),
                    label = j.label.clone()
                ),
                status,
            )
        })
        .collect();
    if rows.is_empty() {
        widgets::caption(ui, t!("connect.joints.none"));
        ui.add_space(theme::SPACE_S);
        if widgets::wide_button(
            ui,
            Kind::Secondary,
            Some(icons::SPLIT),
            &t!("connect.joints.open_split"),
        )
        .clicked()
        {
            app.open_rail_tool(crate::shape_tools::SPLIT);
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
            split_flow::open_joint(app, i);
        }
        widgets::caption(ui, status);
    }
    let Some(joint) = app.split_flow.open_joint().cloned() else {
        widgets::caption(ui, t!("connect.joints.pick"));
        return;
    };
    // allow-swallow: open_joint() above is Some only when joint is set
    let j = app.split_flow.joint.unwrap_or(0);
    let (na, nb) = (name_of(app, joint.a), name_of(app, joint.b));
    // Which piece is see-through.
    section(ui, &t!("connect.section.transparent"));
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
            warning_card(ui, &t!("connect.geometry_failed", reason = e.clone()));
            return;
        }
        (_, None) => {
            widgets::caption(ui, t!("connect.reading"));
            return;
        }
        _ => {}
    }
    widgets::caption(ui, t!("connect.place_help", name = solid_name.clone()));
    // The kind a click places (owner 2026-11-12: the dovetail moved out
    // of this point-placement list into the parametric shape system,
    // `dovetail_tool::place_on_joint`'s shortcut button below; an old
    // joint that already has a `joint.dovetail` placement still shows,
    // edits and replays it unchanged — only *new* placements through
    // this picker can no longer be a dovetail).
    section(ui, &t!("connect.section.kind"));
    let kinds = split_flow::offered_kinds();
    let labels: Vec<String> = kinds.iter().map(|k| split_flow::kind_label(k)).collect();
    let options: Vec<(&str, &str, bool)> = labels
        .iter()
        .map(|l| (l.as_str(), l.as_str(), true))
        .collect();
    let current = kinds
        .iter()
        .position(|k| *k == app.split_flow.kind())
        .unwrap_or(0); // allow-swallow: kind is always one of the offered kinds
    if let (Some(i), _) = widgets::segmented(ui, &options, current) {
        app.split_flow.kind = Some(kinds[i]);
    }
    if ui
        .add_enabled_ui(app.busy.is_none(), |ui| {
            widgets::wide_button(
                ui,
                Kind::Secondary,
                Some(icons::CONNECT),
                &t!("connect.dovetail_shortcut"),
            )
        })
        .inner
        .on_hover_text(t!("connect.dovetail_shortcut.tip"))
        .clicked()
    {
        crate::dovetail_tool::place_on_joint(app, j);
    }
    fit_row(app, ui);
    // Joint-wide checks (D22): about the whole set of connectors, not
    // one row, so they show above the list rather than under a
    // connector that isn't really what they're about.
    let gap_mm = joint.frame.gap / doc_units_per_millimetre(app);
    for jc in mf_operations::joint_checks(&joint.placements, gap_mm) {
        joint_check_card(app, ui, j, &jc);
    }
    // The connectors placed: each a row (click selects it and, if it has
    // warnings, toggles them open here) with its own trash button.
    section(ui, &t!("connect.section.placed"));
    // allow-swallow: checks still computing: no warnings to list yet
    let checks = app.split_flow.checks().unwrap_or_default();
    if joint.placements.is_empty() {
        widgets::caption(ui, t!("connect.placed.none"));
    }
    let mut to_remove = None;
    for (i, p) in joint.placements.iter().enumerate() {
        let n = checks.get(i).map_or(0, Vec::len);
        let label = if n == 0 {
            t!(
                "connect.placed.row_ok",
                n = i + 1,
                kind = split_flow::kind_label(&p.connector)
            )
        } else {
            crate::tp!(
                "connect.placed.row_warn",
                n,
                index = i + 1,
                kind = split_flow::kind_label(&p.connector)
            )
        };
        let on = app.split_flow.selected == Some(i);
        let open = n > 0 && !app.split_flow.collapsed_warnings.contains(&i);
        ui.horizontal(|ui| {
            let row_width = ui.available_width() - theme::ICON_SIZE - theme::SPACE_M * 2.0;
            if widgets::button(
                ui,
                if on { Kind::Primary } else { Kind::Secondary },
                Some(if n == 0 { icons::OK } else { icons::WARNING }),
                &label,
                row_width,
            )
            .clicked()
            {
                app.split_flow.selected = Some(i);
                if n > 0 {
                    if open {
                        app.split_flow.collapsed_warnings.insert(i);
                    } else {
                        app.split_flow.collapsed_warnings.remove(&i);
                    }
                }
            }
            if widgets::icon_button(
                ui,
                icons::TRASH,
                &t!("connect.placed.row_remove", n = i + 1),
                None,
                theme::ICON_SIZE + theme::SPACE_M,
            )
            .clicked()
            {
                to_remove = Some(i);
            }
        });
        if open {
            for c in checks.get(i).into_iter().flatten() {
                check_card(app, ui, j, i, c);
            }
        }
    }
    if let Some(i) = to_remove {
        split_flow::remove_connector(app, j, i);
    }
    // Fix item 9's live warning: a round hole explicitly kept round that
    // prints sideways, with the one-click fix.
    for (i, p) in joint.placements.iter().enumerate() {
        if !sags_sideways(app, &joint, p) {
            continue;
        }
        warning_card(ui, &t!("connect.sideways", n = i + 1));
        if widgets::wide_button(ui, Kind::Secondary, None, &t!("shape.sideways.fix")).clicked()
            && let Some(q) = app
                .split_flow
                .joints
                .get_mut(j)
                .and_then(|jt| jt.placements.get_mut(i))
        {
            *q = mf_operations::patched(
                q,
                &mf_parts::ParamPatch::one(
                    "hole_profile",
                    mf_spec::ParamValue::Text("teardrop".into()),
                ),
            );
        }
    }
    if let Some(i) = app
        .split_flow
        .selected
        .filter(|&i| i < joint.placements.len())
    {
        selected_controls(app, ui, j, i, &joint);
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
            widgets::caption(ui, crate::tp!("connect.apply.later", later));
        } else {
            widgets::caption(ui, t!("connect.apply.replaces"));
        }
    }
    let off_cap = checks
        .iter()
        .flatten()
        .any(|c| c.id == mf_operations::check_id::OFF_CAP);
    let can = app.busy.is_none() && !joint.placements.is_empty() && !off_cap;
    let label = if applied {
        t!("connect.apply.again")
    } else {
        t!("connect.apply")
    };
    if ui
        .add_enabled_ui(can, |ui| {
            widgets::wide_button(ui, Kind::Primary, Some(icons::CONNECT), &label)
        })
        .inner
        .clicked()
    {
        apply_connectors(app, j, label);
    }
    ui.add_space(theme::SPACE_XS);
    if ui
        .add_enabled_ui(can && !applied, |ui| {
            widgets::wide_button(
                ui,
                Kind::Secondary,
                Some(icons::PARTS),
                &t!("connect.as_shapes"),
            )
        })
        .inner
        .on_hover_text(t!("connect.as_shapes.tip"))
        .clicked()
    {
        add_as_shapes(app, j);
    }
}

/// The joint's connectors as linked groups of editable shapes, one group
/// per connector, in one `object.add_shape` step: each part placed where
/// `split.place_connectors` would cut or join it, marked with the piece
/// it joins.
pub fn add_as_shapes(app: &mut MeshFixerApp, j: usize) {
    let Some(joint) = app.split_flow.joints.get(j).cloned() else {
        return;
    };
    let mm = doc_units_per_millimetre(app);
    let gap_mm = joint.frame.gap / mm;
    let mut shapes = Vec::new();
    let first = crate::connector_tool::new_group_id(app, "connect");
    let base: u64 = first
        .rsplit('-')
        .next()
        // allow-swallow: new_group_id always ends in its number
        .and_then(|n| n.parse().ok())
        .unwrap_or(1);
    for (i, p) in joint.placements.iter().enumerate() {
        let params = match mf_operations::placement_params(p, gap_mm) {
            Ok(params) => params,
            Err(e) => return refuse_shapes(app, e.to_string()),
        };
        // `object.add_shape` scales the generator's own mm mesh itself
        // (consolidation plan C12), so the placement's frame is built at
        // scale 1 here (rotation and position only), never pre-scaled by
        // `mm`, or the mesh would be scaled twice.
        let world = mf_operations::placement_to_world(&joint.frame, p, 1.0);
        let transform = [
            world.x_axis.x,
            world.x_axis.y,
            world.x_axis.z,
            world.y_axis.x,
            world.y_axis.y,
            world.y_axis.z,
            world.z_axis.x,
            world.z_axis.y,
            world.z_axis.z,
            world.w_axis.x,
            world.w_axis.y,
            world.w_axis.z,
        ];
        let json = serde_json::to_string(&params).unwrap_or_default(); // allow-swallow: Params is plain data; serializing it can't fail
        let name = format!("{} {}", split_flow::kind_label(&p.connector), i + 1);
        let sides = (joint.piece(p.on), joint.piece(p.on.other()));
        let members = match crate::connector_tool::group_members(
            &p.connector,
            &name,
            &json,
            &transform,
            Some(sides),
        ) {
            Ok(m) => m,
            Err(e) => return refuse_shapes(app, e),
        };
        let group = format!("connect-{}", base + i as u64);
        shapes.extend(members.into_iter().map(|mut m| {
            m.transform = Some(transform);
            m.group = Some(group.clone());
            m
        }));
    }
    if shapes.is_empty() {
        return;
    }
    app.refusal = None;
    let intent = Intent::op("object.add_shape")
        .on(joint.a)
        .param("shapes", mf_operations::shapes::shapes_param(&shapes))
        .param("group", "");
    app.run_intent(intent, t!("connect.shapes_label"));
}

fn refuse_shapes(app: &mut MeshFixerApp, message: String) {
    app.refusal = Some(crate::shape_tools::Refusal {
        op_id: "object.add_shape".into(),
        code: "connector_failed".into(),
        message,
    });
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

/// Connect's own Fit class (Sliding by default) and the Machine
/// profile the fit check uses (the Connectors tool's own choice,
/// shared), and the minimum wall.
fn fit_row(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    section(ui, &t!("connect.section.fit"));
    let (classes, class_err) = crate::connector_tool::fit_classes(app);
    let (profiles, profile_err) = crate::connector_tool::machine_profiles(app);
    if let Some(e) = class_err.or(profile_err) {
        widgets::caption(ui, t!("connect.fit.unreadable", reason = e));
    }
    let mut fit_class = app.split_flow.fit_class();
    egui::ComboBox::from_id_salt("connect fit class")
        .selected_text(t!("connect.fit.class", name = fit_class.clone()))
        .width(ui.available_width())
        .show_ui(ui, |ui| {
            for c in &classes.classes {
                let label = format!("{} ({:+.2} mm)", c.name, c.clearance);
                ui.selectable_value(&mut fit_class, c.name.clone(), label);
            }
        });
    app.split_flow.fit_class = Some(fit_class);
    egui::ComboBox::from_id_salt("connect machine profile")
        .selected_text(t!(
            "connect.fit.machine",
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
    // `min_wall` is print intent, mm (the spec's own `unit: mm`), never
    // document-relative: the same fixed-mm `ScaleCtx` as Connectors'
    // Settings fields (UA §6.4).
    let scale = mf_units::ScaleCtx::new(Unit::Millimeter);
    let opts = LengthFieldOpts {
        range: (
            Length::new(0.2, Unit::Millimeter),
            Length::new(20.0, Unit::Millimeter),
        ),
        scale: &scale,
        label: &t!("connect.min_wall"),
    };
    let mut w = Length::new(app.split_flow.min_wall(), Unit::Millimeter);
    if length_field(ui, egui::Id::unique("connect min wall"), &mut w, &opts).changed() {
        app.split_flow.min_wall = Some(w.mm());
    }
}

/// Params a placement never shows as fields: the joint sets its gap, and
/// the rest are worked out from other choices (the hole profile's
/// resolution and point direction).
const PLACEMENT_DERIVED: &[&str] = &["gap", "profile", "up_x", "up_y"];

fn selected_controls(app: &mut MeshFixerApp, ui: &mut egui::Ui, j: usize, i: usize, joint: &Joint) {
    let p = joint.placements[i].clone();
    let gap_mm = joint.frame.gap / doc_units_per_millimetre(app);
    let spec = mf_parts::find_generator(&p.connector).map(|g| g.spec());
    let options = crate::shape_panel::text_options(app);
    let scale = app.scale_ctx();
    let mut removed = false;
    widgets::card(ui, |ui| {
        ui.label(
            RichText::new(t!(
                "connect.selected",
                n = i + 1,
                kind = split_flow::kind_label(&p.connector)
            ))
            .color(theme::TEXT_STRONG),
        );
        let mut edited = p.clone();
        let mut changed = false;
        // Its turn on the cut face: every placement has one.
        ui.horizontal(|ui| {
            ui.label(RichText::new(t!("connect.param.angle")).color(theme::TEXT_MUTED));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let r = ui.add(
                    egui::DragValue::new(&mut edited.angle)
                        .speed(1.0)
                        .range(ANGLE_RANGE)
                        .suffix("°"),
                );
                changed |= r.changed();
            });
        });
        // Every other param, from its generator's own schema.
        if let (Some(spec), Ok(mut params)) = (spec, mf_operations::placement_params(&p, gap_mm))
            && let Some((name, value)) = crate::shape_panel::param_grid(
                ui,
                ("connect params", i),
                spec,
                &mut params,
                &crate::shape_panel::GridOptions {
                    hidden: PLACEMENT_DERIVED,
                    live: true,
                    idle: true,
                    text_options: &options,
                    scale: &scale,
                },
            )
        {
            edited = mf_operations::patched(&edited, &mf_parts::ParamPatch::one(&name, value));
            changed = true;
        }
        if changed && let Some(jt) = app.split_flow.joints.get_mut(j) {
            jt.placements[i] = edited;
        }
        ui.add_space(theme::SPACE_XS);
        if widgets::wide_button(
            ui,
            Kind::Secondary,
            Some(icons::TRASH),
            &t!("connect.remove"),
        )
        .clicked()
        {
            removed = true;
        }
    });
    if removed {
        split_flow::remove_connector(app, j, i);
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
                    RichText::new(t!("connect.check", n = i + 1, message = c.message.clone()))
                        .color(theme::TEXT),
                )
                .wrap(),
            );
        });
        if let Some(label) = &c.fix_label {
            fix = widgets::wide_button(ui, Kind::Secondary, None, label).clicked();
        }
    });
    ui.add_space(theme::SPACE_XS);
    if fix {
        apply_fix(app, j, i, &c.id);
    }
}

/// Apply check `id`'s one-click fix to connector `i` of joint `j`.
pub fn apply_fix(app: &mut MeshFixerApp, j: usize, i: usize, id: &str) {
    let (Some(joint), Some(g)) = (
        app.split_flow.joints.get(j).cloned(),
        app.split_flow.geometry(),
    ) else {
        return;
    };
    let Some(p) = joint.placements.get(i).cloned() else {
        return;
    };
    let s = split_flow::settings(app);
    let ctx = mf_runtime::RunContext::none();
    let names = [name_of(app, joint.a), name_of(app, joint.b)];
    let fixed = {
        let pc = split_flow::pieces(&names, &g);
        mf_operations::fix(&joint.frame, [&pc[0], &pc[1]], &p, id, &s, &ctx)
    };
    match fixed {
        Ok(Some(q)) => {
            if let Some(jt) = app.split_flow.joints.get_mut(j) {
                jt.placements[i] = q;
            }
            app.split_flow.selected = Some(i);
        }
        Ok(None) => app.status = Some(t!("connect.fix.none")),
        Err(e) => app.status = Some(t!("connect.fix.failed", reason = e.to_string())),
    }
}

/// A joint-wide warning (D22): not about one connector row, so it has
/// no index of its own and no `connect.check` "Connector N:" prefix;
/// one or more fixes, each its own button with its own words.
fn joint_check_card(
    app: &mut MeshFixerApp,
    ui: &mut egui::Ui,
    j: usize,
    jc: &mf_operations::JointCheck,
) {
    let mut fix = None;
    widgets::card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(icons::WARNING)
                    .font(icons::font(theme::ICON_SIZE))
                    .color(theme::LOOSE_PIECE),
            );
            ui.add(egui::Label::new(RichText::new(&jc.message).color(theme::TEXT)).wrap());
        });
        for f in &jc.fixes {
            if widgets::wide_button(ui, Kind::Secondary, None, &f.label).clicked() {
                fix = Some(f.id.clone());
            }
        }
    });
    ui.add_space(theme::SPACE_XS);
    if let Some(id) = fix {
        apply_joint_fix(app, j, &id);
    }
}

/// Apply a joint-wide check's one-click fix (see
/// [`mf_operations::fix_joint`]): the solid piece's own cap bounds the
/// second dowel's placement, so this needs the joint's geometry read in
/// (same as a per-connector fix).
fn apply_joint_fix(app: &mut MeshFixerApp, j: usize, name: &str) {
    let (Some(joint), Some(g)) = (
        app.split_flow.joints.get(j).cloned(),
        app.split_flow.geometry(),
    ) else {
        return;
    };
    let gap_mm = joint.frame.gap / doc_units_per_millimetre(app);
    let cap = g.cap(joint.solid());
    match mf_operations::fix_joint(&joint.placements, gap_mm, cap, name) {
        Some(placements) => {
            let n = placements.len();
            if let Some(jt) = app.split_flow.joints.get_mut(j) {
                jt.placements = placements;
            }
            if n > joint.placements.len() {
                app.split_flow.selected = Some(n - 1);
            }
        }
        None => app.status = Some(t!("connect.fix.none")),
    }
}

/// Apply: `split.place_connectors` with every connector on the joint,
/// one command. A joint whose connectors are already applied is applied
/// again from the revision before them.
fn apply_connectors(app: &mut MeshFixerApp, j: usize, label: String) {
    let Some(joint) = app.split_flow.joints.get(j).cloned() else {
        return;
    };
    if applied_now(app, &joint)
        && let Some((before, _)) = joint.applied
    {
        app.session.checkout(before);
    }
    let s = split_flow::settings(app);
    let (profiles, _) = crate::connector_tool::machine_profiles(app);
    let plate = profiles.get(&app.connectors.machine_profile).map_or(
        mf_parts::MachineProfile::uncalibrated().print_volume_mm,
        |p| p.print_volume_mm,
    );
    let connectors = serde_json::to_string(&joint.placements).unwrap_or_default(); // allow-swallow: Placement is plain data; serializing it can't fail
    // The joint's frame and gap are already in document units (`unit:
    // document`); the print-intent sizes (wall, clearances, plate) are
    // physical millimetres, said so on the `Intent`.
    let mm = |v: f64| Length::new(v, Unit::Millimeter);
    let intent = Intent::op("split.place_connectors")
        .on(joint.a)
        .operand("side_b", joint.b, None)
        .set_doc("plane_point", vec3(joint.frame.point))
        .param("plane_normal", vec3(joint.frame.normal))
        .set_doc("plane_u", vec3(joint.frame.u))
        .set_doc("gap", joint.frame.gap)
        .param("connectors", connectors)
        .set("min_wall", mm(s.min_wall))
        .set("fit_clearance", mm(s.fit_class.clearance))
        .set("machine_tolerance", mm(s.machine.hole_undersize))
        .set("plate_width", mm(plate[0]))
        .set("plate_depth", mm(plate[1]));
    app.refusal = None;
    app.split_flow.pending_apply = Some(j);
    app.run_intent(intent, label);
    if app.busy.is_none() {
        app.split_flow.pending_apply = None;
    }
}

fn vec3(v: DVec3) -> String {
    format!("{},{},{}", v.x, v.y, v.z)
}

// ---------------------------------------------------------------------
// The view: picking a joint's cut face, placing, dragging, handles.
// ---------------------------------------------------------------------

/// What Connect's input and drawing need of a frame, copied out so the
/// view's claim test can run without the app.
#[derive(Clone)]
struct PlaceView {
    cam: mf_render::OrbitCamera,
    size: (f64, f64),
    frame: mf_operations::JointFrame,
    solid: JointSide,
    cap: CapRegion,
    /// Each connector's centre and reach on the cap (2D), its reach along
    /// its own direction (where the rim handle sits), and its handles'
    /// screen positions.
    connectors: Vec<([f64; 2], f64)>,
    reach_x: Vec<f64>,
    selected: Option<usize>,
    handles: Option<((f64, f64), (f64, f64))>,
    /// The other joints: their frames and where their pieces overlap
    /// (world bounds), to pick one by clicking its cut face.
    others: Vec<(usize, mf_operations::JointFrame, DVec3, DVec3)>,
}

impl PlaceView {
    fn of(app: &MeshFixerApp) -> Option<PlaceView> {
        if app.active_tool != CONNECT {
            return None;
        }
        let f = &app.split_flow;
        let joint = f.open_joint()?;
        let g = f.geometry()?;
        let (cam, size) = split_flow::view(app)?;
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
        let scale = doc_units_per_millimetre(app);
        let gap_mm = joint.frame.gap / scale;
        let outlines: Vec<Option<Vec<[f64; 2]>>> = joint
            .placements
            .iter()
            .map(|p| {
                let params = mf_operations::placement_params(p, gap_mm).ok()?; // allow-swallow: an invalid placement is still pickable at a 1 mm reach; its check card says why
                mf_operations::footprint_2d(p, &params, scale).ok() // allow-swallow: as above
            })
            .collect();
        let connectors: Vec<([f64; 2], f64)> = joint
            .placements
            .iter()
            .zip(&outlines)
            .map(|(p, outline)| {
                let reach = outline.as_ref().map_or(scale, |pts| {
                    pts.iter()
                        .map(|q| ((q[0] - p.at[0]).powi(2) + (q[1] - p.at[1]).powi(2)).sqrt())
                        .fold(0.0, f64::max)
                });
                (p.at, reach)
            })
            .collect();
        // Along each placement's own direction: how far its outline
        // reaches (the rim handle's place, and what it resizes).
        let reach_x: Vec<f64> = joint
            .placements
            .iter()
            .zip(&outlines)
            .map(|(p, outline)| {
                let (s, c) = p.angle.to_radians().sin_cos();
                outline.as_ref().map_or(scale, |pts| {
                    pts.iter()
                        .map(|q| (q[0] - p.at[0]) * c + (q[1] - p.at[1]) * s)
                        .fold(0.0, f64::max)
                })
            })
            .collect();
        let selected = f.selected.filter(|&i| i < connectors.len());
        let handles = selected.and_then(|i| {
            let at = connectors[i].0;
            let reach = reach_x[i];
            let p = joint.placements[i].angle.to_radians();
            let rim = [at[0] + reach * p.cos(), at[1] + reach * p.sin()];
            let w = joint.frame.at(rim) + (joint.frame.cap_point(solid) - joint.frame.point);
            let s = split_flow::screen(&cam, size, w)?;
            Some((s, (s.0 + 16.0, s.1)))
        });
        Some(PlaceView {
            cam,
            size,
            frame: joint.frame,
            solid,
            cap,
            connectors,
            reach_x,
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

/// The param a placement's handle sets (`Generator::handles`, picked by
/// `which`) and its current value (mm).
fn handle_value(
    p: &Placement,
    gap_mm: f64,
    which: impl Fn(mf_parts::Handles) -> Option<&'static str>,
) -> Option<(&'static str, f64)> {
    let name = which(mf_parts::find_generator(&p.connector)?.handles())?;
    let params = mf_operations::placement_params(p, gap_mm).ok()?; // allow-swallow: an invalid placement has no handle to drag; its check card says why
    Some((name, params.get_float(name).ok()?)) // allow-swallow: a handle param that isn't a number can't be dragged
}

fn claims(app: &MeshFixerApp) -> Option<crate::shell::Claims> {
    if app.busy.is_some() {
        return None;
    }
    let state = PlaceView::of(app)?;
    Some(Box::new(move |px: (f64, f64)| state.hit(px).is_some()) as crate::shell::Claims)
}

fn input(app: &mut MeshFixerApp, input: &crate::shell::ToolInput) -> Input {
    use crate::viewport::ToolPointer;
    let Some(state) = PlaceView::of(app) else {
        app.split_flow.hover = None;
        return Input::Pass;
    };
    // allow-swallow: PlaceView::of needs an open joint, so joint is set
    let j = app.split_flow.joint.unwrap_or(0);
    let scale = doc_units_per_millimetre(app);
    match input.pointer {
        Some(ToolPointer::Start { from, at: _ }) => {
            app.split_flow.drag = match state.hit(from) {
                Some(Hit::Size(i)) => Some(Drag::Size { index: i }),
                Some(Hit::Depth(i)) => {
                    let p = &app.split_flow.joints[j].placements[i];
                    handle_value(p, app.split_flow.joints[j].frame.gap / scale, |h| h.depth).map(
                        |(_, depth)| Drag::Depth {
                            index: i,
                            from_y: from.1,
                            depth,
                        },
                    )
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
                // A kind placed as a line is dragged across the cut face
                // (`PlacementHint::DragLine`, D23): the press starts the
                // line; every other kind places on `Click` below.
                Some(Hit::Cap(uv))
                    if mf_parts::find_generator(app.split_flow.kind()).is_some_and(|g| {
                        g.placement_hint() == mf_parts::PlacementHint::DragLine
                    }) =>
                {
                    Some(Drag::PlaceLine { start: uv })
                }
                _ => None,
            };
        }
        Some(ToolPointer::Move(px)) | Some(ToolPointer::End(px)) => {
            let drag = app.split_flow.drag;
            if let Some(Drag::PlaceLine { start }) = drag {
                if matches!(input.pointer, Some(ToolPointer::End(_))) {
                    let end = state.on_plane(px).unwrap_or(start);
                    app.split_flow.drag = None;
                    add_line(app, start, end);
                }
                app.split_flow.hover = None;
                return Input::Consumed;
            }
            let gap_mm = app.split_flow.joints[j].frame.gap / scale;
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
                    // The rim follows the pointer: the generator's size
                    // param grows by however much the outline's reach
                    // along its direction has to change to get there.
                    if let (Some(uv), Some(p)) =
                        (state.on_plane(px), joint.placements.get_mut(index))
                        && let Some((name, value)) = handle_value(p, gap_mm, |h| h.size)
                    {
                        let r = ((uv[0] - p.at[0]).powi(2) + (uv[1] - p.at[1]).powi(2)).sqrt();
                        let reach = state.reach_x.get(index).copied().unwrap_or(r); // allow-swallow: index is the dragged placement's, always in range
                        let v = (value + 2.0 * (r - reach) / scale).clamp(0.5, 200.0);
                        p.params.insert(name.into(), (v * 10.0).round() / 10.0);
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
                    if let Some(p) = joint.placements.get_mut(index)
                        && let Some((name, _)) = handle_value(p, gap_mm, |h| h.depth)
                    {
                        let d = (depth + (from_y - px.1) * per_pt).clamp(0.5, 500.0);
                        p.params.insert(name.into(), (d * 10.0).round() / 10.0);
                    }
                }
                // Handled above (returns early): kept here only so the
                // match stays exhaustive as `Drag` grows.
                Some(Drag::PlaceLine { .. }) | None => {}
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
            Some(Hit::Joint(i)) => split_flow::open_joint(app, i),
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

/// The open joint's connectors, over the view, while Connect is open.
pub fn overlay(app: &MeshFixerApp, painter: &egui::Painter, rect: egui::Rect) {
    if app.active_tool != CONNECT {
        return;
    }
    let Some((cam, size)) = split_flow::view(app) else {
        return;
    };
    let at = |p: DVec3| {
        split_flow::screen(&cam, size, p)
            .map(|(x, y)| rect.left_top() + egui::vec2(x as f32, y as f32))
    };
    draw_connectors(app, painter, &at);
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
    let scale = doc_units_per_millimetre(app);
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
    // allow-swallow: checks still computing: no warnings to colour yet
    let checks = f.checks().unwrap_or_default();
    let previews = f
        .previews
        .as_ref()
        .map(|(_, _, p)| p.clone())
        // allow-swallow: previews still computing: nothing to draw yet
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
        // loose dowel; every one flattened onto the solid cap
        // (`flatten_on_cap`, I4) so it draws coincident with its own
        // footprint instead of offset by perspective parallax.
        for part in previews.get(i).into_iter().flatten() {
            let (colour, width) = match part.side {
                Some(s) if s == solid => (own.gamma_multiply(0.45), 1.0),
                Some(_) => (counterpart, if selected { 2.0 } else { 1.4 }),
                None => (theme::TEXT_MUTED.gamma_multiply(0.7), 1.0),
            };
            for e in &part.edges {
                let (a, b) = (
                    split_flow::flatten_on_cap(&joint.frame, solid, e[0]),
                    split_flow::flatten_on_cap(&joint.frame, solid, e[1]),
                );
                if let (Some(a), Some(b)) = (at(a), at(b)) {
                    painter.line_segment([a, b], egui::Stroke::new(width, colour));
                }
            }
        }
        // Its outline on the solid face, filled.
        let outline = mf_operations::placement_params(p, joint.frame.gap / scale)
            // allow-swallow: drawing only: an outline that can't be built isn't drawn; applying reports why
            .ok()
            // allow-swallow: drawing only: an outline that can't be built isn't drawn; applying reports why
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
        // allow-swallow: handles exist only for a selected connector
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
            t!("connect.handle.depth"),
            egui::FontId::proportional(11.0),
            theme::TEXT,
        );
    }
}
