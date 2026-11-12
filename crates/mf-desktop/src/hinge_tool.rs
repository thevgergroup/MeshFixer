//! Hinges between two objects (parametric-shapes brief, "Hinges"):
//! placing a hinge as its three pieces and the swing preview.
//!
//! - **Placement.** Pick object A and object B; the hinge axis snaps to
//!   the edge they share (where their bounding boxes meet), or another
//!   edge of that shared face. Leaf A is marked to join A and leaf B to
//!   join B; the pin stays its own object. Its gaps come from the Fit
//!   class and Machine profile (refused, with the fix, if they'd print
//!   fused). Attach (or Combine) joins each leaf onto its object.
//! - **Swing preview.** With a hinge selected, a slider turns object B
//!   and everything attached to it about the pin axis, 0° to the open
//!   limit. The whole swing is checked on a worker thread with the exact
//!   triangle test (`mf_parts::swing`): the panel says where the two
//!   sides first touch, and the view highlights where they collide at
//!   the slider's angle.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use eframe::egui;
use glam::{DMat4, DVec3};
use mf_command::{GROUP, ObjectId, RevisionId};
use mf_parts::swing::{SwingBody, SwingReport};

use crate::MeshFixerApp;
use crate::widgets::{self, Kind};
use crate::{icons, t, theme};

/// The hinge connector's id.
pub const HINGE: &str = "hinge.leaf";

/// A worker's swing check: `None` while it runs.
pub type SwingResult = Arc<Mutex<Option<Result<SwingReport, String>>>>;

/// The hinge placement choices and the swing preview.
#[derive(Debug, Clone, Default)]
pub struct HingeState {
    pub a: Option<ObjectId>,
    pub b: Option<ObjectId>,
    /// Which of [`edges`] the axis snaps to (0: the shared edge).
    pub edge: usize,
    pub swing: Option<Swing>,
}

/// The swing preview of one placed hinge.
#[derive(Debug, Clone)]
pub struct Swing {
    pub group: String,
    pub revision: RevisionId,
    /// The slider, degrees.
    pub angle: f64,
    pub limit: f64,
    /// The objects that turn (object B and what's attached to it).
    pub moving: Vec<ObjectId>,
    pub origin: DVec3,
    pub dir: DVec3,
    pub result: SwingResult,
}

/// One edge a hinge can sit on: its name and the hinge frame there (3MF
/// rows: local x along the edge, y from A to B, z out of the face, the
/// origin at the edge's middle on the face), and its length.
#[derive(Debug, Clone, PartialEq)]
pub struct HingeEdge {
    pub label: String,
    pub frame: [f64; 12],
    pub length: f64,
}

fn axis_name(k: usize, sign: f64) -> String {
    format!(
        "{}{}",
        if sign > 0.0 { "+" } else { "−" },
        ["X", "Y", "Z"][k]
    )
}

/// The edges objects `a` and `b` share, the best first: where their
/// world boxes meet (the axis they're separated along), the four edges
/// of the face they share, longest first and the `+` side before the
/// `−`. Empty if their boxes overlap in every axis (no face between
/// them).
pub fn edges(app: &MeshFixerApp, a: ObjectId, b: ObjectId) -> Vec<HingeEdge> {
    let (Some((alo, ahi)), Some((blo, bhi))) = (
        crate::shape_tools::world_bounds(app, a),
        crate::shape_tools::world_bounds(app, b),
    ) else {
        return Vec::new();
    };
    // The separating axis: the one with the largest separation (gap
    // between the boxes, or the least overlap).
    let sep = |k: usize| (alo[k] - bhi[k]).max(blo[k] - ahi[k]);
    let k = (0..3)
        .max_by(|&i, &j| sep(i).total_cmp(&sep(j)))
        .expect("three axes");
    let size = (ahi - alo).max(bhi - blo).max_element().max(1e-9);
    if sep(k) < -0.05 * size {
        return Vec::new();
    }
    let toward_b = if blo[k] >= ahi[k] - 0.05 * size {
        1.0
    } else {
        -1.0
    };
    let contact = if toward_b > 0.0 {
        (ahi[k] + blo[k]) / 2.0
    } else {
        (alo[k] + bhi[k]) / 2.0
    };
    let e = |i: usize| {
        let mut v = DVec3::ZERO;
        v[i] = 1.0;
        v
    };
    let others = [(k + 1) % 3, (k + 2) % 3];
    let mut out = Vec::new();
    for along in others {
        let across = others[0] + others[1] - along;
        let lo = alo[along].max(blo[along]);
        let hi = ahi[along].min(bhi[along]);
        let (clo, chi) = (alo[across].max(blo[across]), ahi[across].min(bhi[across]));
        if hi <= lo || chi <= clo {
            continue;
        }
        for (face, out_sign) in [(chi, 1.0), (clo, -1.0)] {
            let y = e(k) * toward_b;
            let z = e(across) * out_sign;
            // Right-handed: x × y = z, so x runs along the edge one way
            // or the other.
            let x = y.cross(z);
            let mut origin = DVec3::ZERO;
            origin[k] = contact;
            origin[across] = face;
            origin[along] = (lo + hi) / 2.0;
            let length = hi - lo;
            let frame = [
                x.x, x.y, x.z, y.x, y.y, y.z, z.x, z.y, z.z, origin.x, origin.y, origin.z,
            ];
            out.push(HingeEdge {
                label: t!(
                    "hinge.edge",
                    face = axis_name(across, out_sign),
                    along = ["X", "Y", "Z"][along]
                ),
                frame,
                length,
            });
        }
    }
    out.sort_by(|p, q| q.length.total_cmp(&p.length));
    out
}

/// The hinge's resolved gaps from the chosen Fit class and Machine
/// profile: (gap between the moving parts, radial clearance around the
/// pin, the class's and profile's names).
pub fn hinge_fit(app: &MeshFixerApp) -> Result<(f64, f64, String, String), String> {
    let (class, machine) = crate::connector_tool::chosen_fit(app)?;
    Ok((
        class.clearance + machine.outer_xy,
        class.clearance + machine.hole_undersize,
        class.name,
        machine.name,
    ))
}

/// Places a hinge between `a` and `b` on `edge`: its three pieces as a
/// linked group (`object.add_shape`, one undo step), leaf A marked to
/// join `a` and leaf B `b`. Its length is the edge's, up to 30 mm,
/// centred on it.
pub fn place_hinge(app: &mut MeshFixerApp, a: ObjectId, b: ObjectId, edge: &HingeEdge) {
    let (gap, clearance, class, machine) = match hinge_fit(app) {
        Ok(f) => f,
        Err(e) => {
            return refuse(app, "fit_settings_unreadable", e);
        }
    };
    let Some(connector) = mf_parts::connectors().into_iter().find(|c| c.id() == HINGE) else {
        return;
    };
    let length = (edge.length * 0.8).clamp(4.0, 30.0);
    let raw = BTreeMap::from([
        ("gap".to_owned(), gap.to_string()),
        ("pin_clearance".to_owned(), clearance.to_string()),
        ("length".to_owned(), length.to_string()),
        ("fit_class".to_owned(), class),
        ("machine_profile".to_owned(), machine),
    ]);
    let params = match connector.spec().validate(&raw) {
        Ok(p) => p,
        Err(e) => return refuse(app, "invalid_parameter", e.to_string()),
    };
    // The frame's origin is the edge's middle: the hinge runs from x = 0,
    // so start it half its length back.
    let mut frame = edge.frame;
    for c in 0..3 {
        frame[9 + c] -= frame[c] * length / 2.0;
    }
    let json = serde_json::to_string(&params).unwrap_or_default(); // allow-swallow: Params is plain data; serializing it can't fail
    let members = match crate::connector_tool::group_members(
        HINGE,
        &t!("hinge.name"),
        &json,
        &frame,
        Some((a, b)),
    ) {
        Ok(m) => m,
        Err(e) => return refuse(app, "moving_parts_fused", e),
    };
    let group = crate::connector_tool::new_group_id(app, HINGE);
    app.refusal = None;
    app.run_invocation(
        mf_command::Invocation {
            op_id: "object.add_shape".into(),
            target: a,
            selection: None,
            operands: Vec::new(),
            raw_params: BTreeMap::from([
                (
                    "shapes".to_owned(),
                    mf_operations::shapes::shapes_param(&members),
                ),
                (
                    "transform".to_owned(),
                    frame.map(|v| v.to_string()).join(","),
                ),
                ("group".to_owned(), group),
            ]),
        },
        t!("hinge.place"),
    );
}

fn refuse(app: &mut MeshFixerApp, code: &str, message: String) {
    app.refusal = Some(crate::shape_tools::Refusal {
        op_id: "object.add_shape".into(),
        code: code.into(),
        message,
    });
}

/// The Connectors panel's hinge section: pick A and B, the edge, place.
pub fn placement_section(app: &mut MeshFixerApp, ui: &mut egui::Ui) {
    widgets::section(ui, &t!("hinge.section"));
    widgets::caption(ui, t!("hinge.help"));
    let objects: Vec<(ObjectId, String)> = app
        .session
        .document()
        .objects()
        .iter()
        .filter(|o| !o.properties.contains_key(GROUP))
        .map(|o| (o.id, o.name.clone()))
        .collect();
    for (which, label) in [(0, t!("hinge.object_a")), (1, t!("hinge.object_b"))] {
        ui.label(label);
        ui.horizontal_wrapped(|ui| {
            for (id, name) in &objects {
                let slot = if which == 0 {
                    &mut app.hinge.a
                } else {
                    &mut app.hinge.b
                };
                if ui.selectable_label(*slot == Some(*id), name).clicked() {
                    *slot = Some(*id);
                    app.hinge.edge = 0;
                }
            }
        });
    }
    let (Some(a), Some(b)) = (app.hinge.a, app.hinge.b) else {
        return;
    };
    if a == b {
        widgets::caption(ui, t!("hinge.same_object"));
        return;
    }
    let edges = edges(app, a, b);
    if edges.is_empty() {
        widgets::caption(ui, t!("hinge.no_edge"));
        return;
    }
    ui.label(t!("hinge.axis"));
    ui.horizontal_wrapped(|ui| {
        for (i, e) in edges.iter().enumerate() {
            let label = if i == 0 {
                t!("hinge.shared_edge", edge = e.label.clone())
            } else {
                e.label.clone()
            };
            if ui.selectable_label(app.hinge.edge == i, label).clicked() {
                app.hinge.edge = i;
            }
        }
    });
    let edge = edges[app.hinge.edge.min(edges.len() - 1)].clone();
    let fit = hinge_fit(app);
    let fused = match &fit {
        Ok((gap, clearance, class, _)) if *gap <= 0.0 || *clearance <= 0.0 => {
            widgets::caption(
                ui,
                t!(
                    "hinge.too_tight",
                    class = class.clone(),
                    gap = format!("{:.2}", gap.min(*clearance))
                ),
            );
            if widgets::wide_button(ui, Kind::Secondary, None, &t!("hinge.use_free_running"))
                .clicked()
            {
                app.connectors.fit_class = mf_parts::FitClass::builtins()[3].name.clone();
            }
            true
        }
        Ok((gap, clearance, _, _)) => {
            widgets::caption(
                ui,
                t!(
                    "hinge.fit",
                    gap = format!("{gap:.2}"),
                    clearance = format!("{clearance:.2}")
                ),
            );
            false
        }
        Err(e) => {
            widgets::caption(ui, e.clone());
            true
        }
    };
    if ui
        .add_enabled_ui(app.busy.is_none() && !fused, |ui| {
            widgets::wide_button(
                ui,
                Kind::Primary,
                Some(icons::CONNECTORS),
                &t!("hinge.place"),
            )
        })
        .inner
        .clicked()
    {
        place_hinge(app, a, b, &edge);
    }
}

fn mat(t: &[f64; 12]) -> DMat4 {
    DMat4::from_cols_array(&[
        t[0], t[1], t[2], 0.0, t[3], t[4], t[5], 0.0, t[6], t[7], t[8], 0.0, t[9], t[10], t[11],
        1.0,
    ])
}

fn rows(m: &DMat4) -> [f64; 12] {
    [
        m.x_axis.x, m.x_axis.y, m.x_axis.z, m.y_axis.x, m.y_axis.y, m.y_axis.z, m.z_axis.x,
        m.z_axis.y, m.z_axis.z, m.w_axis.x, m.w_axis.y, m.w_axis.z,
    ]
}

/// What a hinge group's swing turns and about what: (the hinge's own
/// params' open limit, the axis in world space, the fixed bodies' ids,
/// the moving bodies' ids). `None` unless `id` is in a hinge group whose
/// leaves know their objects.
#[allow(clippy::type_complexity)]
pub fn swing_setup(
    app: &MeshFixerApp,
    id: ObjectId,
) -> Option<(String, f64, DVec3, DVec3, Vec<ObjectId>, Vec<ObjectId>)> {
    let (group, a, b, _) = crate::shape_panel::attach_sides(app, id)?;
    let members = crate::shape_panel::members(app, id);
    let (leaf, record) = members
        .iter()
        .find(|(_, r)| r.id == HINGE && r.part.as_deref() == Some("leaf_a"))?;
    let doc = app.session.document();
    let t = mat(&doc.object(*leaf)?.transform);
    let (origin, dir) = mf_parts::connectors::leaf_hinge::axis(&record.params).ok()?;
    let origin = t.transform_point3(DVec3::from(origin));
    let dir = t.transform_vector3(DVec3::from(dir)).normalize_or(DVec3::X);
    let limit = record.params.get_float("open_limit").unwrap_or(180.0);
    let joins = |o: &mf_command::Object, to: ObjectId| {
        o.properties.get(mf_operations::shapes::ATTACH_TO) == Some(&to.0.to_string())
    };
    let mut fixed = vec![a];
    let mut moving = vec![b];
    for o in doc.objects() {
        if joins(o, a) {
            fixed.push(o.id);
        } else if joins(o, b) {
            moving.push(o.id);
        } else if o.properties.get(GROUP) == Some(&group) {
            // The pin (it joins neither side) turns with neither.
            fixed.push(o.id);
        }
    }
    Some((group, limit, origin, dir, fixed, moving))
}

/// Starts (or keeps) the swing check for the hinge `id` belongs to.
fn ensure_swing(app: &mut MeshFixerApp, id: ObjectId) -> bool {
    let Some((group, limit, origin, dir, fixed, moving)) = swing_setup(app, id) else {
        app.hinge.swing = None;
        return false;
    };
    let revision = app.session.current();
    if let Some(s) = &app.hinge.swing
        && s.group == group
        && s.revision == revision
    {
        return true;
    }
    let doc = app.session.document();
    let body = |o: ObjectId| -> Option<SwingBody> {
        let o = doc.object(o)?;
        Some(SwingBody {
            mesh: o.mesh()?.clone(),
            to_world: mat(&o.transform),
        })
    };
    let fixed: Vec<SwingBody> = fixed.iter().filter_map(|&o| body(o)).collect();
    let moving_bodies: Vec<SwingBody> = moving.iter().filter_map(|&o| body(o)).collect();
    let result: SwingResult = Arc::new(Mutex::new(None));
    let out = result.clone();
    let repaint = app.repaint_context();
    std::thread::spawn(move || {
        let ctx = mf_runtime::RunContext::none();
        let r = mf_parts::swing::swing(&fixed, &moving_bodies, origin, dir, limit, 2.0, &ctx)
            .map_err(|_| "cancelled".to_owned());
        *out.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
        if let Some(c) = repaint {
            c.request_repaint();
        }
    });
    let angle = app
        .hinge
        .swing
        .as_ref()
        .filter(|s| s.group == group)
        .map_or(0.0, |s| s.angle.min(limit));
    app.hinge.swing = Some(Swing {
        group,
        revision,
        angle,
        limit,
        moving,
        origin,
        dir,
        result,
    });
    true
}

/// The swing check's report, once the worker has it.
pub fn swing_report(app: &MeshFixerApp) -> Option<Result<SwingReport, String>> {
    let s = app.hinge.swing.as_ref()?;
    s.result.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The shape panel's swing section, for a selected hinge.
pub fn swing_section(app: &mut MeshFixerApp, ui: &mut egui::Ui, id: ObjectId) {
    if !ensure_swing(app, id) {
        return;
    }
    widgets::section(ui, &t!("hinge.swing"));
    let report = swing_report(app);
    let Some(s) = app.hinge.swing.as_mut() else {
        return;
    };
    let limit = s.limit;
    ui.add(
        egui::Slider::new(&mut s.angle, 0.0..=limit)
            .suffix("°")
            .max_decimals(0)
            .text(t!("hinge.swing.open")),
    );
    match report {
        None => {
            widgets::caption(ui, t!("hinge.swing.checking"));
        }
        Some(Err(e)) => {
            widgets::caption(ui, e);
        }
        Some(Ok(r)) => {
            match r.first_contact_deg {
                None => widgets::body(ui, t!("hinge.swing.clear", limit = format!("{limit:.0}"))),
                Some(a) => widgets::chip(
                    ui,
                    icons::WARNING,
                    theme::HOLE,
                    &t!("hinge.swing.first_contact", angle = format!("{a:.0}")),
                ),
            };
            let at = r.contacts_at(s.angle);
            if !at.is_empty() {
                widgets::caption(ui, t!("hinge.swing.colliding", n = at.len()));
            }
        }
    }
}

/// `id`'s transform turned by the swing preview, if it's on the moving
/// side of the hinge being previewed.
pub fn swung(app: &MeshFixerApp, id: ObjectId) -> Option<[f64; 12]> {
    let s = app.hinge.swing.as_ref()?;
    if s.angle == 0.0 || !s.moving.contains(&id) || app.session.current() != s.revision {
        return None;
    }
    let t = app.session.document().object(id)?.transform;
    let turned = mf_parts::swing::rotation_about(s.origin, s.dir, s.angle) * mat(&t);
    Some(rows(&turned))
}

/// Highlights where the two sides collide at the slider's angle.
pub fn overlay(app: &MeshFixerApp, painter: &egui::Painter, _rect: egui::Rect) {
    let Some(s) = app.hinge.swing.as_ref() else {
        return;
    };
    if app.session.current() != s.revision {
        return;
    }
    let Some(Ok(r)) = swing_report(app) else {
        return;
    };
    let stroke = egui::Stroke::new(3.0, theme::HOLE);
    for c in r.contacts_at(s.angle) {
        for seg in &c.segments {
            if let (Some(p), Some(q)) = (app.screen_point(seg[0]), app.screen_point(seg[1])) {
                painter.line_segment([p, q], stroke);
            }
        }
        if let Some(p) = app.screen_point(c.centroid) {
            painter.circle_stroke(p, 10.0, stroke);
        }
    }
}
