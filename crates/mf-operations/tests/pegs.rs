#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Alignment holes and dowels for `mesh.split_plane` (Phase 1 Task 13,
//! D12, D21). The metre-document default and replay go through the
//! command layer (`mf-command/tests/split.rs`).

use std::collections::BTreeMap;

use mf_document::ObjectId;
use mf_fixtures::{cube, scaled, uv_sphere};
use mf_geometry::{Mesh, inspect};
use mf_io::threemf::IDENTITY;
use mf_operations::layout::{Plate, layout_on_plate};
use mf_operations::{
    DocOutput, ObjectChange, OpError, OperandInput, RunContext, Target, dowel_size, find,
};

fn split(mesh: &Mesh, params: &[(&str, &str)]) -> Result<DocOutput, OpError> {
    let spec = mf_spec::builtin::get("mesh.split_plane").unwrap();
    let raw: BTreeMap<String, String> = params
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    find("mesh.split_plane").unwrap().execute_doc(
        &[OperandInput {
            object: ObjectId(1),
            name: "scan",
            mesh,
            transform: IDENTITY,
            target: Target::WholeObject,
        }],
        &spec.validate(&raw).unwrap(),
        &RunContext::none(),
    )
}

fn sphere() -> Mesh {
    scaled(&uv_sphere(48, 96), 30.0)
}

struct Out {
    top: Mesh,
    bottom: Mesh,
    /// Corners of the faces the holes made (created with no source).
    top_hole: Vec<[f64; 3]>,
    bottom_hole: Vec<[f64; 3]>,
    dowels: Vec<(Mesh, [f64; 12])>,
    report: serde_json::Value,
}

fn parts(out: DocOutput) -> Out {
    let mut top = None;
    let mut bottom = None;
    let mut dowels = Vec::new();
    let hole = |m: &Mesh, p: &mf_geometry::Provenance| -> Vec<[f64; 3]> {
        p.created_faces
            .iter()
            .filter(|(_, s)| s.faces.is_empty() && s.operands.is_empty())
            .flat_map(|(f, _)| m.face(*f).unwrap().map(|v| m.corner_position(v)))
            .collect()
    };
    for c in out.changes {
        match c {
            ObjectChange::Geometry {
                operand: 0,
                mesh,
                provenance,
            } => top = Some((hole(&mesh, &provenance), mesh)),
            ObjectChange::Create {
                name,
                mesh,
                transform,
                ..
            } if name.contains("dowel") => dowels.push((mesh, transform)),
            ObjectChange::Create {
                mesh, provenance, ..
            } => bottom = Some((hole(&mesh, &provenance), mesh)),
            _ => {}
        }
    }
    let (top_hole, top) = top.unwrap();
    let (bottom_hole, bottom) = bottom.unwrap();
    Out {
        top,
        bottom,
        top_hole,
        bottom_hole,
        dowels,
        report: out.report,
    }
}

/// The cut is 0.37 above the equator: the UV sphere's equator ring sits
/// within rounding of z = 0, which the plane cut refuses as grazing.
const DOWELS: [(&str, &str); 4] = [
    ("pegs", "dowels"),
    ("hole_depth_above", "5"),
    ("hole_depth_below", "8"),
    ("plane_point", "0,0,0.37"),
];

/// Largest distance from the z axis through (cx, cy) of `mesh`'s vertices
/// within `r_max` of it and in `z0..z1`.
fn radius_near(mesh: &Mesh, c: [f64; 2], r_max: f64, z0: f64, z1: f64) -> f64 {
    mesh.vertices()
        .map(|(_, p)| (((p[0] - c[0]).powi(2) + (p[1] - c[1]).powi(2)).sqrt(), p[2]))
        .filter(|&(r, z)| r <= r_max && z > z0 && z < z1)
        .map(|(r, _)| r)
        .fold(0.0, f64::max)
}

#[test]
fn dowel_size_matches_owner_example() {
    let (d, len) = dowel_size(5.0, 5.0, 8.0, 0.2, 0.4);
    assert!((len - 12.6).abs() < 1e-12, "{len}");
    assert!((d - 4.8).abs() < 1e-12, "{d}");
}

#[test]
fn dowels_measured_on_meshes_fit_their_holes() {
    let out = parts(split(&sphere(), &DOWELS).unwrap());
    assert_eq!(out.dowels.len(), 2, "{}", out.report);
    // The dowel: diameter and length measured on its mesh.
    let (dowel, _) = &out.dowels[0];
    let bb = dowel.bounding_box().unwrap();
    assert!((bb.max[2] - bb.min[2] - 12.6).abs() < 1e-9);
    let dowel_r = radius_near(dowel, [0.0, 0.0], 10.0, 0.2, 12.4);
    assert!((2.0 * dowel_r - 4.8).abs() < 1e-9, "{dowel_r}");
    // Each hole: its rim (on the cut) and floor vertices are the part's
    // vertices well inside the section, between the cut and the hole's
    // depth (the prism has corners only there).
    let tol = 0.02; // a tenth of the clearance
    let cut = 0.37;
    let e = 1e-9;
    for (hole, z0, z1, depth) in [
        (&out.bottom_hole, cut - 8.0 - e, cut + e, 8.0),
        (&out.top_hole, cut - e, cut + 5.0 + e, 5.0),
    ] {
        let wall: Vec<[f64; 3]> = hole
            .iter()
            .copied()
            .filter(|p| p[2] > z0 && p[2] < z1)
            .collect();
        assert!(!wall.is_empty());
        // Two holes on opposite sides along one axis: split by sign of
        // the projection on the first vertex's direction.
        let a = [wall[0][0], wall[0][1]];
        for side in [1.0, -1.0] {
            let pts: Vec<&[f64; 3]> = wall
                .iter()
                .filter(|p| side * (p[0] * a[0] + p[1] * a[1]) > 0.0)
                .collect();
            // The hole's axis: the middle of its corners' bounds.
            let lo = pts
                .iter()
                .fold([f64::INFINITY; 2], |a, p| [a[0].min(p[0]), a[1].min(p[1])]);
            let hi = pts.iter().fold([f64::NEG_INFINITY; 2], |a, p| {
                [a[0].max(p[0]), a[1].max(p[1])]
            });
            let c = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0];
            let r = pts
                .iter()
                .map(|p| ((p[0] - c[0]).powi(2) + (p[1] - c[1]).powi(2)).sqrt())
                .fold(0.0, f64::max);
            let measured = 2.0 * r - 4.8;
            assert!((measured - 0.2).abs() <= tol, "clearance {measured}");
        }
        // Depth: the hole's floor, from the cut.
        let deepest = wall.iter().map(|p| (p[2] - cut).abs()).fold(0.0, f64::max);
        assert!((deepest - depth).abs() < 1e-9, "depth {deepest} vs {depth}");
    }
}

#[test]
fn dowels_are_separate_objects_on_the_plate_not_overlapping_anything() {
    let out = parts(split(&sphere(), &DOWELS).unwrap());
    let mut prints = vec![[-30.0, -30.0, 30.0, 30.0]];
    for (mesh, t) in &out.dowels {
        let bb = mesh.bounding_box().unwrap();
        assert_eq!(bb.min[2] + t[11], 0.0, "world min z");
        let f = [
            bb.min[0] + t[9],
            bb.min[1] + t[10],
            bb.max[0] + t[9],
            bb.max[1] + t[10],
        ];
        for g in &prints {
            assert!(
                f[2] <= g[0] || g[2] <= f[0] || f[3] <= g[1] || g[3] <= f[1],
                "{f:?} overlaps {g:?}"
            );
        }
        assert!(
            f[0] >= -128.0 && f[2] <= 128.0 && f[1] >= -128.0 && f[3] <= 128.0,
            "on the plate"
        );
        prints.push(f);
    }
}

#[test]
fn every_part_is_closed_and_manifold_after_holes() {
    for pegs in ["dowels", "attached"] {
        let out = parts(
            split(
                &sphere(),
                &[
                    ("pegs", pegs),
                    ("hole_depth_above", "5"),
                    ("hole_depth_below", "8"),
                    ("plane_point", "0,0,0.37"),
                ],
            )
            .unwrap(),
        );
        for m in std::iter::once(&out.top)
            .chain([&out.bottom])
            .chain(out.dowels.iter().map(|d| &d.0))
        {
            let d = inspect(m);
            assert!(
                d.is_closed && d.is_manifold && d.inconsistent_orientation_edges == 0,
                "{pegs}: {d:?}"
            );
            assert!(d.signed_volume.unwrap() > 0.0);
        }
    }
}

#[test]
fn auto_places_two_holes_in_a_wide_section_and_one_keyed_in_a_narrow_one() {
    let wide = parts(split(&sphere(), &DOWELS).unwrap());
    assert_eq!(wide.report["pegs"]["holes"], 2);
    let narrow = parts(
        split(
            &scaled(&cube(1.0), 9.0),
            &[
                ("pegs", "dowels"),
                ("plane_point", "0,0,4.3"),
                ("hole_depth_above", "3"),
                ("hole_depth_below", "3"),
            ],
        )
        .unwrap(),
    );
    assert_eq!(narrow.report["pegs"]["holes"], 1, "{}", narrow.report);
    assert_eq!(narrow.dowels.len(), 1);
    // Keyed: its cross-section isn't round (a flat side).
    let (d, _) = &narrow.dowels[0];
    let bb = d.bounding_box().unwrap();
    assert!(
        (bb.max[0] - bb.min[0]) < (bb.max[1] - bb.min[1]) - 0.5,
        "{bb:?}"
    );
}

#[test]
fn too_thin_section_refuses_with_widest_fit() {
    match split(
        &scaled(&cube(1.0), 4.0),
        &[("pegs", "dowels"), ("plane_point", "0,0,2.1")],
    ) {
        Err(OpError::Failed { code, message }) => {
            assert_eq!(code, "section_too_thin");
            assert!(
                message.contains("the widest hole that fits is 1.00"),
                "{message}"
            );
        }
        other => panic!("{:?}", other.map(|o| o.report)),
    }
}

#[test]
fn full_plate_reports_dowels_off_the_plate() {
    let out = parts(
        split(
            &sphere(),
            &[
                ("pegs", "dowels"),
                ("plate_width", "62"),
                ("plate_depth", "62"),
                ("plane_point", "0,0,0.37"),
            ],
        )
        .unwrap(),
    );
    assert_eq!(out.dowels.len(), 2);
    assert_eq!(out.report["pegs"]["off_plate"].as_array().unwrap().len(), 2);
    assert_eq!(
        out.report["pegs"]["off_plate_message"],
        "2 dowels didn't fit on the plate; they're placed beside it"
    );
    let layout = layout_on_plate(
        &[],
        &[[0.0, 0.0, 10.0, 10.0]; 3],
        &Plate { size: [25.0, 12.0] },
        1.0,
    );
    assert_eq!(layout.off_plate, vec![2]);
}
