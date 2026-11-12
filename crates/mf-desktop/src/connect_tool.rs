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
use mf_command::Invocation;
use mf_operations::{CapRegion, Check, CheckKind, JointSide, Placement};

use crate::MeshFixerApp;
use crate::shell::{Availability, Category, Input, RegisterError, Shell, Tool as T};
use crate::split_flow::{self, Drag, GeomSlot, Hit, Joint, applied_now, exists, mm, name_of};
use crate::widgets::{self, Kind};
use crate::{icons, t, theme};

pub const CONNECT: &str = "connect";

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

/// A connector's parameters the panel edits, per kind: (name, label id).
fn editable(kind: &str) -> &'static [(&'static str, &'static str)] {
    match kind {
        "joint.dowel" => &[
            ("diameter", "connect.param.diameter"),
            ("depth_on", "connect.param.depth_on"),
            ("depth_other", "connect.param.depth_other"),
            ("dowel_length", "connect.param.dowel_length"),
            ("clearance", "connect.param.clearance"),
        ],
        "joint.dovetail" => &[
            ("diameter", "connect.param.width"),
            ("length", "connect.param.length"),
            ("depth_on", "connect.param.root"),
            ("depth_other", "connect.param.depth_other"),
            ("clearance", "connect.param.clearance"),
        ],
        "joint.insert" => &[
            ("diameter", "connect.param.insert"),
            ("screw_diameter", "connect.param.screw"),
            ("depth_on", "connect.param.depth_on"),
            ("depth_other", "connect.param.depth_other"),
            ("clearance", "connect.param.clearance"),
        ],
        "joint.magnet" => &[
            ("diameter", "connect.param.magnet"),
            ("depth_on", "connect.param.depth_on"),
            ("depth_other", "connect.param.depth_other"),
            ("clearance", "connect.param.clearance"),
        ],
        _ => &[
            ("diameter", "connect.param.diameter"),
            ("depth_on", "connect.param.peg_root"),
            ("depth_other", "connect.param.depth_other"),
            ("clearance", "connect.param.clearance"),
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

/// A plain click's default dovetail length each way, mm (owner
/// 2026-11-11, D23): the drag is the primary way to place one (its
/// direction and the boundary it's dragged to set the slide direction
/// and reach exactly), but a click without a drag still has to place
/// something sensible rather than nothing — half of `length`'s own
/// default (16 mm) each way, through both ends, the same "it runs out
/// of the piece on purpose" shape every dovetail had before this (the
/// old `groove_length: 10_000.0` hack), just not actually unbounded
/// (`DEFAULT_CLICK_REACH` is already generous next to any real part).
const DEFAULT_CLICK_REACH: f64 = 50.0;

/// Add a connector of the chosen kind at `at` on the open joint's solid
/// cut face, with the clearance the Fit class and Machine profile ask
/// for; it becomes the picked one. A dovetail placed this way (a click,
/// not a drag) gets a short default reach both ways, through both ends
/// ([`DEFAULT_CLICK_REACH`]); [`add_dovetail`] is the drag path proper.
pub fn add_connector(app: &mut MeshFixerApp, at: [f64; 2]) {
    let kind = app.split_flow.kind();
    let clearance = mf_operations::wanted_clearance(&split_flow::settings(app));
    let mut params = BTreeMap::from([(
        "clearance".to_owned(),
        (clearance * 1000.0).round() / 1000.0,
    )]);
    if kind == "joint.dovetail" {
        params.insert("groove_a".into(), DEFAULT_CLICK_REACH);
        params.insert("groove_b".into(), DEFAULT_CLICK_REACH);
        params.insert("open_a".into(), 1.0);
        params.insert("open_b".into(), 1.0);
    }
    push_placement(app, kind, at, 0.0, params);
}

/// Add a dovetail dragged from `start` to `end` on the open joint's
/// solid cut face (owner 2026-11-11, D23): the line's direction is the
/// slide direction (`angle`, the usual local-x turn every connector
/// already has); the groove reaches from its centre to the solid cap's
/// own outline on each side, along that direction, **through** (so the
/// recorded reach is `OVER_THE_EDGE` past the boundary, same idea as
/// the old `groove_length: 10_000.0` hack, just now a real, finite,
/// recorded number instead of a magic one) unless the drag's own
/// endpoint already fell short of the boundary, in which case that end
/// is **stopped** at exactly the dragged length (the person dragged to
/// a deliberately short reach, so the placement respects that rather
/// than silently extending it to the edge).
pub fn add_dovetail(app: &mut MeshFixerApp, start: [f64; 2], end: [f64; 2]) {
    const OVER_THE_EDGE: f64 = 1000.0;
    let (dx, dy) = (end[0] - start[0], end[1] - start[1]);
    let len = (dx * dx + dy * dy).sqrt();
    let (dir, dragged) = if len > 1e-6 {
        ([dx / len, dy / len], len)
    } else {
        ([1.0, 0.0], 0.0)
    };
    let at = [(start[0] + end[0]) / 2.0, (start[1] + end[1]) / 2.0];
    let half_dragged = dragged / 2.0;
    let scale = mm(app);
    let caps = app.split_flow.geometry();
    let on = app
        .split_flow
        .open_joint()
        .map_or(JointSide::A, |j| j.transparent.other());
    // The groove is cut into the *other* piece from `on`: its cap is
    // the boundary a dragged end can reach (mirrors the live check,
    // `CheckKind::DovetailStopped`, and `generate`'s own `Side::B`
    // groove in `mf_parts::connectors::joint`).
    let groove_cap = caps.as_ref().map(|g| g.cap(on.other()));
    let reach = |sign: f64| {
        let d = [dir[0] * sign, dir[1] * sign];
        let to_edge = groove_cap
            .and_then(|cap| cap.ray_boundary(at, d))
            .map(|t| t / scale);
        match to_edge {
            // The drag already reached (or passed) the boundary on this
            // side: through, recorded well past it.
            Some(edge) if half_dragged >= edge - 1e-6 => (edge + OVER_THE_EDGE, true),
            // The drag stopped short on purpose: stopped, at exactly
            // the dragged length (never less than a sliver, so a
            // same-point click-drag still makes a usable dovetail).
            _ => (half_dragged.max(DEFAULT_CLICK_REACH * 0.2), false),
        }
    };
    let (reach_a, open_a) = reach(-1.0);
    let (reach_b, open_b) = reach(1.0);
    let angle = dir[1].atan2(dir[0]).to_degrees();
    let clearance = mf_operations::wanted_clearance(&split_flow::settings(app));
    let params = BTreeMap::from([
        (
            "clearance".to_owned(),
            (clearance * 1000.0).round() / 1000.0,
        ),
        ("groove_a".to_owned(), (reach_a * 10.0).round() / 10.0),
        ("groove_b".to_owned(), (reach_b * 10.0).round() / 10.0),
        ("open_a".to_owned(), if open_a { 1.0 } else { 0.0 }),
        ("open_b".to_owned(), if open_b { 1.0 } else { 0.0 }),
    ]);
    push_placement(app, "joint.dovetail", at, angle, params);
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
    // The kind a click places.
    section(ui, &t!("connect.section.kind"));
    let kinds = mf_parts::connectors::joint::JOINT_CONNECTORS;
    let labels: Vec<String> = kinds.iter().map(|k| split_flow::kind_label(k)).collect();
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
    // Joint-wide checks (D22): about the whole set of connectors, not
    // one row, so they show above the list rather than under a
    // connector that isn't really what they're about.
    let gap_mm = joint.frame.gap / mm(app);
    for jc in mf_operations::joint_checks(&joint.placements, gap_mm) {
        joint_check_card(app, ui, j, &jc);
    }
    // The connectors placed: each a row (click selects it and, if it has
    // warnings, toggles them open here) with its own trash button.
    section(ui, &t!("connect.section.placed"));
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
    let off_cap = checks.iter().flatten().any(|c| c.kind == CheckKind::OffCap);
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
    let mm = mm(app);
    let gap_mm = joint.frame.gap / mm;
    let mut shapes = Vec::new();
    let first = crate::connector_tool::new_group_id(app, "connect");
    let base: u64 = first
        .rsplit('-')
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or(1); // allow-swallow: new_group_id always ends in its number
    for (i, p) in joint.placements.iter().enumerate() {
        let params = match mf_operations::placement_params(p, gap_mm) {
            Ok(params) => params,
            Err(e) => return refuse_shapes(app, e.to_string()),
        };
        let world = mf_operations::placement_to_world(&joint.frame, p, mm);
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
    app.run_invocation(
        Invocation {
            op_id: "object.add_shape".into(),
            target: joint.a,
            selection: None,
            operands: Vec::new(),
            raw_params: BTreeMap::from([
                (
                    "shapes".to_owned(),
                    mf_operations::shapes::shapes_param(&shapes),
                ),
                ("group".to_owned(), String::new()),
            ]),
        },
        t!("connect.shapes_label"),
    );
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
    ui.horizontal(|ui| {
        ui.label(RichText::new(t!("connect.min_wall")).color(theme::TEXT_MUTED));
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

fn selected_controls(app: &mut MeshFixerApp, ui: &mut egui::Ui, j: usize, i: usize, joint: &Joint) {
    let p = joint.placements[i].clone();
    let gap_mm = joint.frame.gap / mm(app);
    let full = full_params(&p, gap_mm);
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
                ui.label(RichText::new(t!("connect.param.angle")).color(theme::TEXT_MUTED));
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
            // Each end's reach and whether it's through (open) or
            // stopped short of the edge on purpose (D23): `a` is the
            // drag line's start side (-x, local frame), `b` its end (+x).
            for (reach_key, open_key, label, through_label) in [
                (
                    "groove_a",
                    "open_a",
                    t!("connect.param.groove_a"),
                    t!("connect.param.through_a"),
                ),
                (
                    "groove_b",
                    "open_b",
                    t!("connect.param.groove_b"),
                    t!("connect.param.through_b"),
                ),
            ] {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(label).color(theme::TEXT_MUTED));
                    let mut open = full.get(open_key).copied().unwrap_or(1.0) != 0.0;
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let mut reach = full.get(reach_key).copied().unwrap_or(0.0);
                        if !open {
                            let r = ui.add(
                                egui::DragValue::new(&mut reach)
                                    .speed(0.5)
                                    .range(0.0..=1000.0)
                                    .suffix(" mm")
                                    .max_decimals(1),
                            );
                            if r.changed() {
                                edited.params.insert(reach_key.to_owned(), reach);
                                changed = true;
                            }
                        }
                        if ui.checkbox(&mut open, through_label).changed() {
                            edited
                                .params
                                .insert(open_key.to_owned(), if open { 1.0 } else { 0.0 });
                            changed = true;
                        }
                    });
                });
            }
        }
        if p.connector == "joint.dowel" || p.connector == "joint.peg" {
            // The cross-section (D22, anti-rotation): round, square, an
            // N-sided polygon (with its own `sides` field, 3..=12) or
            // keyed. `mf_parts::connectors::joint::Shape` is the single
            // source of truth for the encoding; the panel only ever
            // reads/writes the same `shape`/`sides` params it records.
            ui.label(RichText::new(t!("connect.param.shape")).color(theme::TEXT_MUTED));
            {
                let code = full.get("shape").copied().unwrap_or(0.0);
                let sides = full.get("sides").copied().unwrap_or(6.0);
                let shape = mf_parts::connectors::joint::Shape::from_code(code, sides);
                let labels = [
                    t!("connect.shape.round"),
                    t!("connect.shape.square"),
                    t!("connect.shape.nsided"),
                    t!("connect.shape.keyed"),
                ];
                let options: Vec<(&str, &str, bool)> = labels
                    .iter()
                    .map(|l| (l.as_str(), l.as_str(), true))
                    .collect();
                let current = shape.code() as usize;
                if let (Some(i), _) = widgets::segmented(ui, &options, current) {
                    edited.params.insert("shape".to_owned(), i as f64);
                    changed = true;
                }
            }
            if matches!(
                mf_parts::connectors::joint::Shape::from_code(
                    full.get("shape").copied().unwrap_or(0.0),
                    full.get("sides").copied().unwrap_or(6.0),
                ),
                mf_parts::connectors::joint::Shape::NSided { .. }
            ) {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(t!("connect.param.sides")).color(theme::TEXT_MUTED));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let mut sides = full.get("sides").copied().unwrap_or(6.0);
                        let r = ui.add(
                            egui::DragValue::new(&mut sides)
                                .speed(1.0)
                                .range(3.0..=12.0),
                        );
                        if r.changed() {
                            edited.params.insert("sides".to_owned(), sides.round());
                            changed = true;
                        }
                    });
                });
            }
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
        let label = match c.kind {
            CheckKind::Breakout(_) => t!("connect.fix.breakout"),
            CheckKind::EdgeDistance | CheckKind::OffCap => t!("connect.fix.inwards"),
            CheckKind::DowelLength => t!("connect.fix.dowel"),
            CheckKind::Fit => t!("connect.fix.fit"),
            CheckKind::DovetailStopped(_) => t!("connect.fix.dovetail_edge"),
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
    let s = split_flow::settings(app);
    let ctx = mf_runtime::RunContext::none();
    let names = [name_of(app, joint.a), name_of(app, joint.b)];
    let fixed = {
        let pc = split_flow::pieces(&names, &g);
        mf_operations::fix(&joint.frame, [&pc[0], &pc[1]], &p, kind, &s, &ctx)
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
/// one or more named fixes, each its own button (today's only one,
/// `SingleRoundDowel`, offers two).
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
        for &name in &jc.fixes {
            let label = match name {
                "add_second_dowel" => t!("connect.fix.add_second_dowel"),
                "make_square" => t!("connect.fix.make_square"),
                _ => continue,
            };
            if widgets::wide_button(ui, Kind::Secondary, None, &label).clicked() {
                fix = Some(name);
            }
        }
    });
    ui.add_space(theme::SPACE_XS);
    if let Some(name) = fix {
        apply_joint_fix(app, j, name);
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
    let gap_mm = joint.frame.gap / mm(app);
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
    /// Each connector's centre and reach on the cap (2D), and its handles'
    /// screen positions.
    connectors: Vec<([f64; 2], f64)>,
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
                // A dovetail is placed by dragging a line across the
                // cut face, not a single click (owner 2026-11-11, D23):
                // the press starts the line; every other kind still
                // places on `Click` below, unchanged.
                Some(Hit::Cap(uv)) if app.split_flow.kind() == "joint.dovetail" => {
                    Some(Drag::PlaceDovetail { start: uv })
                }
                _ => None,
            };
        }
        Some(ToolPointer::Move(px)) | Some(ToolPointer::End(px)) => {
            let drag = app.split_flow.drag;
            if let Some(Drag::PlaceDovetail { start }) = drag {
                if matches!(input.pointer, Some(ToolPointer::End(_))) {
                    let end = state.on_plane(px).unwrap_or(start);
                    app.split_flow.drag = None;
                    add_dovetail(app, start, end);
                }
                app.split_flow.hover = None;
                return Input::Consumed;
            }
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
                // Handled above (returns early): kept here only so the
                // match stays exhaustive as `Drag` grows.
                Some(Drag::PlaceDovetail { .. }) | None => {}
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
            t!("connect.handle.depth"),
            egui::FontId::proportional(11.0),
            theme::TEXT,
        );
    }
}
