//! The Transform tool (MF-OBJ-001, Phase 1 Task 5): the gizmo in the 3D
//! view, numeric fields, Drop to plate and Reset.
//!
//! Everything here is small `f64` arithmetic on the UI thread: a drag
//! changes only the dragged object's model matrix (a uniform), and its
//! release commits one `object.set_transform` with the absolute resulting
//! matrix (D7). Nothing reads the mesh: the pivot is the object's
//! bounding-box centre, from bounds the analysis worker measured, and the
//! plate snap's lowest point is found on a worker too.

use glam::{DMat3, DVec3};
use mf_command::ObjectId;
use mf_io::threemf::Unit;
use mf_render::{Decomposed, Drag, Gizmo, GizmoMode, Handle, compose, decompose};

use crate::defects::{measure, unit_symbol};

/// The tool's catalogue id (`tools::CATALOGUE`).
pub const ID: &str = "transform";

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

/// What the status line says after a placement changed from `from` to
/// `to`: "Moved scan by 12.0, 0, 0 mm", "Rotated plate 90° about Z",
/// "Scaled part by 150%".
pub fn describe(name: &str, unit: Unit, from: &[f64; 12], to: &[f64; 12], centre: DVec3) -> String {
    let (l0, _) = parts(from);
    let (l1, _) = parts(to);
    let close = |a: DMat3, b: DMat3| (a - b).to_cols_array().iter().all(|v| v.abs() < 1e-12);
    if close(l0, l1) {
        let d = world_point(to, centre) - world_point(from, centre);
        return format!(
            "Moved {name} by {}, {}, {} {}",
            number(d.x),
            number(d.y),
            number(d.z),
            unit_symbol(unit)
        );
    }
    if l0.determinant().abs() > 1e-300 {
        let rel = l1 * l0.inverse();
        let ortho = close(rel.transpose() * rel, DMat3::IDENTITY) && rel.determinant() > 0.0;
        if ortho {
            let q = glam::DQuat::from_mat3(&rel);
            let (axis, angle) = q.to_axis_angle();
            for (k, e) in [DVec3::X, DVec3::Y, DVec3::Z].into_iter().enumerate() {
                let along = axis.dot(e);
                if (along.abs() - 1.0).abs() < 1e-9 {
                    return format!(
                        "Rotated {name} {}° about {}",
                        degrees(angle.to_degrees() * along.signum()),
                        AXIS_NAMES[k]
                    );
                }
            }
            return format!("Rotated {name} {}°", degrees(angle.to_degrees()));
        }
    }
    let s0 = [l0.col(0).length(), l0.col(1).length(), l0.col(2).length()];
    let s1 = [l1.col(0).length(), l1.col(1).length(), l1.col(2).length()];
    let pct: Vec<f64> = (0..3).map(|k| 100.0 * s1[k] / s0[k].max(1e-300)).collect();
    if (pct[0] - pct[1]).abs() < 1e-9 && (pct[1] - pct[2]).abs() < 1e-9 {
        format!("Scaled {name} by {}%", degrees(pct[0]))
    } else {
        format!(
            "Scaled {name} by {}%, {}%, {}%",
            degrees(pct[0]),
            degrees(pct[1]),
            degrees(pct[2])
        )
    }
}

/// A length in the status line: 3 significant figures, and 0 for a
/// rounding speck.
fn number(v: f64) -> String {
    if v.abs() < 1e-9 {
        "0".into()
    } else {
        measure(v)
    }
}

/// An angle or percentage: whole when it is whole, else one decimal.
fn degrees(v: f64) -> String {
    if (v - v.round()).abs() < 0.05 {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.1}")
    }
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

/// The smallest a zoom-adaptive grid step's on-screen size is let fall
/// (screen points): below this a step reads as "not really a grid"
/// (ADR-0006 rule 3: "at least about 8 pt"). Matches the plate snap's
/// own 8pt catch distance ([`mf_render::PlateSnap::within_pt`]), a
/// separate, pre-existing feature this isn't replacing. `pub(crate)` so
/// `app::MeshFixerApp::plate`'s own zoom-adaptive grid-line spacing
/// shares this one constant too, rather than a second "8.0" literal
/// (consolidation plan C09: "delete duplicate step logic").
pub(crate) const MIN_SNAP_PX: f64 = 8.0;

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
            describe("plate", Unit::Millimeter, &IDENTITY, &t, c),
            "Rotated plate 90° about Z"
        );
    }

    #[test]
    fn status_words_for_move_and_scale() {
        let c = DVec3::ZERO;
        let moved = with_position(&IDENTITY, c, DVec3::new(12.0, 0.0, 0.0));
        assert_eq!(
            describe("scan", Unit::Millimeter, &IDENTITY, &moved, c),
            "Moved scan by 12.0, 0, 0 mm"
        );
        let scaled = with_rotation_scale(&IDENTITY, c, DVec3::ZERO, DVec3::splat(1.5)).unwrap();
        assert_eq!(
            describe("part", Unit::Millimeter, &IDENTITY, &scaled, c),
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
