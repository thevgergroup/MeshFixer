//! The dovetail as a linked parametric pair (owner decision 2026-11-12:
//! moved out of Connect's point placement into the parametric shape
//! system, `dovetail.slide`, `mf_parts::connectors::dovetail_slide`).
//!
//! - **Placement.** From the Parts library or the Connectors tool, like
//!   any other connector group (`object.add_shape`, one undo step): a
//!   solid tail on the piece it's placed on, a hole groove in the other,
//!   marked to join each.
//! - **Connect's shortcut.** The Dovetail button on a selected joint
//!   drops the linked pair aligned to the cap plane, pointing to the
//!   nearest edge of the solid piece's cap, through
//!   [`place_on_joint`].
//! - **Extend through edge.** On by default (the spec's own
//!   `extend_through_edge`): whenever the group moves or turns
//!   (`object.set_transform`, `object.move_group`), [`after_commit`]
//!   re-evaluates the tail's length so it clears the piece it's attached
//!   to by about a millimetre, the same idle-reconciliation pattern a
//!   hole profile's re-orientation already uses
//!   (`shape_panel::after_commit`). A stopped or short dovetail that
//!   doesn't exit gets the "ends inside the piece" warning
//!   ([`warning_card`]) with a one-click fix that turns
//!   `extend_through_edge` back on.

use std::collections::BTreeMap;

use eframe::egui;
use glam::{DMat3, DVec3};
use mf_command::ObjectId;
use mf_parts::GeneratorRecord;
use mf_parts::connectors::dovetail_slide::{exits_piece, extended_length};

use crate::MeshFixerApp;
use crate::widgets::{self, Kind};
use crate::{icons, t, theme};

/// The dovetail connector's id.
pub const DOVETAIL: &str = "dovetail.slide";

/// The tail's local x axis, and the target's bounding box projected onto
/// it, in the tail's own local frame (so [`extended_length`] and
/// [`exits_piece`] can work in pure local coordinates): `None` if the
/// tail object or its attach target have no mesh to measure.
fn target_span_local(app: &MeshFixerApp, tail: ObjectId, target: ObjectId) -> Option<(f64, f64)> {
    let doc = app.session.document();
    let tail_t = doc.object(tail)?.transform;
    let target_o = doc.object(target)?;
    let target_mesh = target_o.mesh()?;
    let bb = target_mesh.bounding_box()?;
    // World-to-local: the tail's own placement, inverted (rigid plus
    // uniform scale; a mirror or shear would still invert correctly
    // through the same affine).
    let tail_rot = crate::shape_panel::rotation(&tail_t);
    let tail_pos = DVec3::new(tail_t[9], tail_t[10], tail_t[11]);
    let to_local = DMat3::from_cols(tail_rot.x_axis, tail_rot.y_axis, tail_rot.z_axis).inverse();
    let target_rot = crate::shape_panel::rotation(&target_o.transform);
    let target_pos = DVec3::new(
        target_o.transform[9],
        target_o.transform[10],
        target_o.transform[11],
    );
    // Every corner of the target's own bounding box, in the tail's local
    // frame: the box's own rotation means the world AABB can't be
    // recovered from its corners alone, so each of the 8 corners is
    // carried through both transforms and the local x extent taken over
    // all of them.
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for i in 0..8u32 {
        let corner = DVec3::new(
            if i & 1 == 0 { bb.min[0] } else { bb.max[0] },
            if i & 2 == 0 { bb.min[1] } else { bb.max[1] },
            if i & 4 == 0 { bb.min[2] } else { bb.max[2] },
        );
        let world = target_rot * corner + target_pos;
        let local_x = to_local.mul_vec3(world - tail_pos).x;
        lo = lo.min(local_x);
        hi = hi.max(local_x);
    }
    Some((lo, hi))
}

/// `id`'s group, if it's a dovetail tail (the member with `part ==
/// "tail"`), the tail's own object id and recipe, and the object it's
/// attached to (its `attach_to` property): `None` for anything else, or
/// a tail whose attach target has gone.
fn dovetail_tail(
    app: &MeshFixerApp,
    id: ObjectId,
) -> Option<(ObjectId, GeneratorRecord, ObjectId)> {
    let members = crate::shape_panel::members(app, id);
    let (tail_id, record) = members
        .iter()
        .find(|(_, r)| r.id == DOVETAIL && r.part.as_deref() == Some("tail"))?;
    let doc = app.session.document();
    let target = doc
        .object(*tail_id)?
        .properties
        .get(mf_operations::shapes::ATTACH_TO)?
        .parse::<u64>()
        .ok()
        .map(ObjectId)?;
    doc.object(target)?;
    Some((*tail_id, record.clone(), target))
}

/// Re-evaluates a dovetail's `extend_through_edge` after `id`'s group
/// moved or turned: `None` if `id` isn't a dovetail tail, extending is
/// off, or it's already long enough. Otherwise, the tail's (and the
/// groove's, the same `length`) new recipe.
pub fn reextend(app: &MeshFixerApp, id: ObjectId) -> Option<(ObjectId, GeneratorRecord)> {
    let (tail_id, record, target) = dovetail_tail(app, id)?;
    if !record.params.get_bool("extend_through_edge").unwrap_or(true) {
        return None;
    }
    let (lo, hi) = target_span_local(app, tail_id, target)?;
    let current = record.params.get_float("length").unwrap_or(0.0);
    let wanted = extended_length(current, lo, hi);
    if (wanted - current).abs() < 1e-6 {
        return None;
    }
    let mut params = record.params.clone();
    params
        .0
        .insert("length".into(), mf_spec::ParamValue::Float(wanted));
    Some((tail_id, GeneratorRecord { params, ..record }))
}

/// Whether the dovetail `id` belongs to (any member of its group) ends
/// inside the piece it's attached to: `through` is set but the tail's
/// current length doesn't clear the target's boundary on either end
/// (the "ends inside the piece" warning). `None` if `id` isn't a
/// dovetail, or the check can't be made (no attach target yet).
pub fn ends_inside(app: &MeshFixerApp, id: ObjectId) -> Option<bool> {
    let (tail_id, record, target) = dovetail_tail(app, id)?;
    if !record.params.get_bool("through").unwrap_or(true) {
        return None; // stopped on purpose: not a warning
    }
    let (lo, hi) = target_span_local(app, tail_id, target)?;
    let length = record.params.get_float("length").unwrap_or(0.0);
    Some(!exits_piece(length, lo, hi))
}

/// The shape panel's dovetail section, for a selected tail or groove:
/// the "ends inside the piece" warning with its one-click fix.
pub fn warning_section(app: &mut MeshFixerApp, ui: &mut egui::Ui, id: ObjectId) {
    if ends_inside(app, id) != Some(true) {
        return;
    }
    widgets::chip(
        ui,
        icons::WARNING,
        theme::HOLE,
        &t!("dovetail.ends_inside"),
    );
    if ui
        .add_enabled_ui(app.busy.is_none(), |ui| {
            widgets::wide_button(ui, Kind::Secondary, None, &t!("dovetail.ends_inside.fix"))
        })
        .inner
        .clicked()
    {
        crate::shape_panel::set_param(
            app,
            "extend_through_edge",
            mf_spec::ParamValue::Bool(true),
        );
    }
}

/// The dovetail's resolved clearance from the chosen Fit class and
/// Machine profile (D20): (fit_clearance, machine_tolerance, the class's
/// and profile's names).
fn dovetail_fit(app: &MeshFixerApp) -> Result<(f64, f64, String, String), String> {
    let (class, machine) = crate::connector_tool::chosen_fit(app)?;
    Ok((
        class.clearance,
        machine.hole_undersize,
        class.name,
        machine.name,
    ))
}

/// Places a dovetail between `a` (the tail's piece) and `b` (the
/// groove's piece) at `frame` (3MF rows: local x the slide direction,
/// z the cut's normal from a into b), `length` long: Connect's shortcut
/// and a plain by-hand placement both call this.
pub fn place_dovetail(
    app: &mut MeshFixerApp,
    a: ObjectId,
    b: ObjectId,
    frame: [f64; 12],
    length: f64,
) {
    let (fit_clearance, machine_tolerance, class, machine) = match dovetail_fit(app) {
        Ok(f) => f,
        Err(e) => return refuse(app, "fit_settings_unreadable", e),
    };
    let Some(connector) = mf_parts::connectors().into_iter().find(|c| c.id() == DOVETAIL) else {
        return;
    };
    let raw = BTreeMap::from([
        ("length".to_owned(), length.to_string()),
        ("fit_clearance".to_owned(), fit_clearance.to_string()),
        (
            "machine_tolerance".to_owned(),
            machine_tolerance.to_string(),
        ),
        ("fit_class".to_owned(), class),
        ("machine_profile".to_owned(), machine),
    ]);
    let params = match connector.spec().validate(&raw) {
        Ok(p) => p,
        Err(e) => return refuse(app, "invalid_parameter", e.to_string()),
    };
    let json = serde_json::to_string(&params).unwrap_or_default(); // allow-swallow: Params is plain data; serializing it can't fail
    let members = match crate::connector_tool::group_members(
        DOVETAIL,
        &t!("dovetail.name"),
        &json,
        &frame,
        Some((a, b)),
    ) {
        Ok(m) => m,
        Err(e) => return refuse(app, "connector_failed", e),
    };
    let group = crate::connector_tool::new_group_id(app, DOVETAIL);
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
        t!("dovetail.place"),
    );
}

fn refuse(app: &mut MeshFixerApp, code: &str, message: String) {
    app.refusal = Some(crate::shape_tools::Refusal {
        op_id: "object.add_shape".into(),
        code: code.into(),
        message,
    });
}

/// Connect's Dovetail shortcut: drops the linked pair onto the open
/// joint's cap plane, the tail on the solid (opaque) side, pointing at
/// the nearest edge of that side's own cap outline (the slide direction
/// runs from the cap's centre towards the closest point on its
/// boundary, so the dovetail starts aimed outwards, towards the edge it
/// most likely needs to extend through).
pub fn place_on_joint(app: &mut MeshFixerApp, j: usize) {
    let Some(joint) = app.split_flow.joints.get(j).cloned() else {
        return;
    };
    let Some(g) = app.split_flow.geometry() else {
        return;
    };
    let solid = joint.solid();
    let cap = g.cap(solid);
    // The cap outline's own bounds' midpoint and the nearest boundary
    // point to it, in the cap's local (u, v): the slide direction points
    // from one to the other, so the dovetail aims outwards across the
    // piece, towards its nearest edge.
    let Some((lo, hi)) = cap.bounds() else {
        return;
    };
    let centre = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0];
    let Some((_, nearest)) = cap.nearest_boundary(centre) else {
        return;
    };
    let dir = [nearest[0] - centre[0], nearest[1] - centre[1]];
    let len = (dir[0] * dir[0] + dir[1] * dir[1]).sqrt();
    let dir = if len > 1e-6 {
        [dir[0] / len, dir[1] / len]
    } else {
        [1.0, 0.0]
    };
    let scale = crate::split_flow::mm(app);
    let angle = dir[1].atan2(dir[0]);
    let frame = mf_operations::placement_to_world(
        &joint.frame,
        &mf_operations::Placement {
            connector: DOVETAIL.to_owned(),
            on: solid,
            at: centre,
            angle: angle.to_degrees(),
            params: BTreeMap::new(),
        },
        scale,
    );
    let rows = [
        frame.x_axis.x,
        frame.x_axis.y,
        frame.x_axis.z,
        frame.y_axis.x,
        frame.y_axis.y,
        frame.y_axis.z,
        frame.z_axis.x,
        frame.z_axis.y,
        frame.z_axis.z,
        frame.w_axis.x,
        frame.w_axis.y,
        frame.w_axis.z,
    ];
    let (a, b) = (joint.piece(solid), joint.piece(solid.other()));
    // A short default length; `extend_through_edge` (on by default)
    // grows it to clear the piece as soon as it's placed and re-aimed
    // (`reextend`, run once the group exists and has a real bounding
    // box to measure against).
    place_dovetail(app, a, b, rows, 16.0);
}
