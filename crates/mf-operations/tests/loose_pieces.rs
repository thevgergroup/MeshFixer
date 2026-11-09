#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The loose-piece safeguard (MF-ISSUE-052): "small" is judged against the
//! largest piece as well as by a triangle count, so the defaults leave a
//! design made of many parts (or unwelded soup) alone, while the specks
//! around a scan are still removed. What a run would remove is previewed
//! from the component sizes alone, and a removal that takes more than half
//! of the pieces or more than 10% of the triangles needs confirmation.

use std::collections::BTreeMap;

use mf_fixtures::{cube, merge, translated, uv_sphere};
use mf_geometry::Mesh;
use mf_operations::{RemovalPreview, RunContext, Target, find, removal_preview};
use mf_spec::{Params, builtin};

fn params(pairs: &[(&str, &str)]) -> Params {
    let raw: BTreeMap<String, String> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    builtin::get("mesh.remove_small_components")
        .unwrap()
        .validate(&raw)
        .unwrap()
}

fn sizes(mesh: &Mesh) -> Vec<usize> {
    mesh.topology().components.iter().map(Vec::len).collect()
}

/// What the defaults remove from `mesh`, run for real, with its preview.
fn default_run(mesh: &Mesh) -> (u64, u64, RemovalPreview) {
    let p = params(&[]);
    let op = find("mesh.remove_small_components").unwrap();
    let out = op
        .execute(mesh, &Target::WholeObject, &p, &RunContext::none())
        .unwrap();
    let preview = removal_preview(&sizes(mesh), &p).unwrap();
    (
        out.report["removed_components"].as_u64().unwrap(),
        out.report["removed_triangles"].as_u64().unwrap(),
        preview,
    )
}

/// A 1,512-triangle sphere, five 12-triangle cubes (0.8% of it: specks)
/// and a 480-triangle part (a third of it: part of the design).
fn body_specks_and_part() -> Mesh {
    let mut parts = vec![uv_sphere(28, 28)];
    for i in 0..5 {
        parts.push(translated(&cube(0.1), [5.0 + i as f64, 0.0, 0.0]));
    }
    parts.push(translated(&uv_sphere(16, 16), [0.0, 9.0, 0.0]));
    merge(&parts)
}

#[test]
fn relative_size_keeps_parts_comparable_to_the_main_body() {
    let mesh = body_specks_and_part();
    let s = sizes(&mesh);
    assert_eq!(s.len(), 7, "{s:?}");
    // An absolute threshold above the part lets it through; the relative
    // limit (1% of 1,512 triangles) stops it, but not the specks.
    let p = params(&[("max_triangles", "1000")]);
    let op = find("mesh.remove_small_components").unwrap();
    let out = op
        .execute(&mesh, &Target::WholeObject, &p, &RunContext::none())
        .unwrap();
    assert_eq!(out.report["removed_components"], 5, "only the specks");
    assert_eq!(out.report["removed_triangles"], 60);
    let after = out.change.unwrap().0;
    assert_eq!(sizes(&after).len(), 2);
    // At 100% the relative limit is off: the part goes too.
    let p = params(&[("max_triangles", "1000"), ("max_relative_size", "100")]);
    let out = op
        .execute(&mesh, &Target::WholeObject, &p, &RunContext::none())
        .unwrap();
    assert_eq!(out.report["removed_components"], 6);
}

#[test]
fn the_preview_matches_the_run_and_asks_before_large_removals() {
    let mesh = body_specks_and_part();
    let (pieces, triangles, preview) = default_run(&mesh);
    assert_eq!(
        preview,
        RemovalPreview {
            pieces: 5,
            of_pieces: 7,
            triangles: 60,
            of_triangles: s_total(&mesh),
        }
    );
    assert_eq!((pieces, triangles), (5, 60));
    // Five of seven pieces is more than half: Apply asks first.
    assert!(preview.needs_confirmation(), "{preview:?}");
    // More than half of the pieces needs confirmation…
    let many = RemovalPreview {
        pieces: 4,
        of_pieces: 7,
        triangles: 1,
        of_triangles: 1000,
    };
    assert!(many.needs_confirmation());
    // …and so does more than 10% of the triangles.
    let heavy = RemovalPreview {
        pieces: 1,
        of_pieces: 7,
        triangles: 101,
        of_triangles: 1000,
    };
    assert!(heavy.needs_confirmation());
    let fine = RemovalPreview {
        pieces: 3,
        of_pieces: 7,
        triangles: 100,
        of_triangles: 1000,
    };
    assert!(!fine.needs_confirmation());
}

fn s_total(mesh: &Mesh) -> u64 {
    mesh.triangle_count() as u64
}

/// Records made before `max_relative_size` existed (operation version 1)
/// replay without the relative limit, exactly as they ran.
#[test]
fn version_1_records_replay_without_the_relative_limit() {
    let op = find("mesh.remove_small_components").unwrap();
    assert_eq!(op.version(), 2);
    let mut old = Params(
        [
            ("max_triangles".to_owned(), mf_spec::ParamValue::Int(1000)),
            ("keep_largest".to_owned(), mf_spec::ParamValue::Bool(true)),
        ]
        .into_iter()
        .collect(),
    );
    assert!(op.upgrade_params(1, &mut old));
    assert_eq!(old.get_float("max_relative_size").unwrap(), 100.0);
    let out = op
        .execute(
            &body_specks_and_part(),
            &Target::WholeObject,
            &old,
            &RunContext::none(),
        )
        .unwrap();
    assert_eq!(out.report["removed_components"], 6, "as version 1 did");
    // An unknown version isn't guessed at.
    let mut future = params(&[]);
    assert!(!op.upgrade_params(3, &mut future));
}

fn real(name: &str, test: &str) -> Option<Mesh> {
    let path = mf_fixtures::real::path(name, test)?;
    let mut model = mf_io::read_model(&path).unwrap();
    Some(model.objects.swap_remove(0).mesh)
}

/// MF-ISSUE-052: on the multi-part dress and the unwelded soup, the
/// defaults no longer delete most of the model without asking: the dress's
/// 1,050 trim parts (under 1% of its body each) and the soup's 1,972
/// single triangles are still candidates, but taking more than half of
/// the pieces asks for confirmation first.
#[test]
fn defaults_keep_the_design_parts_of_messy_models() {
    for file in [
        "messy-verlan-dress-285440.obj",
        "messy-openrc-differential-100727.stl",
    ] {
        let Some(mesh) = real(file, "defaults_keep_the_design_parts_of_messy_models") else {
            continue;
        };
        let (pieces, triangles, preview) = default_run(&mesh);
        assert_eq!(
            (preview.pieces, preview.triangles),
            (pieces, triangles),
            "{file}: the preview is what the run does"
        );
        println!("{file}: {preview:?}");
        assert!(
            preview.needs_confirmation()
                || (2 * pieces <= preview.of_pieces && 10 * triangles <= preview.of_triangles),
            "{file}: the defaults remove {pieces} of {} pieces ({triangles} of {} triangles) \
             without asking",
            preview.of_pieces,
            preview.of_triangles
        );
    }
}

/// The specks around a real scan are still removed by the defaults.
#[test]
fn scan_specks_are_still_removed_by_default() {
    let Some(mesh) = real(
        "messy-site3-scan-85538.stl",
        "scan_specks_are_still_removed_by_default",
    ) else {
        return;
    };
    let (pieces, triangles, preview) = default_run(&mesh);
    println!("site3: {preview:?}");
    assert!(pieces > 0, "the specks are removed");
    assert!(10 * triangles <= preview.of_triangles, "{preview:?}");
}

/// The indexed preview agrees with running the operation, for any
/// thresholds (it answers in O(log n) for a panel's sliders).
#[test]
fn the_indexed_preview_agrees_with_the_run() {
    let mesh = body_specks_and_part();
    let index = mf_operations::ComponentSizes::new(&sizes(&mesh));
    let op = find("mesh.remove_small_components").unwrap();
    for max in ["1", "5", "13", "100", "481", "2000"] {
        for rel in ["0", "0.5", "1", "40", "100"] {
            for keep in ["true", "false"] {
                let p = params(&[
                    ("max_triangles", max),
                    ("max_relative_size", rel),
                    ("keep_largest", keep),
                ]);
                let out = op
                    .execute(&mesh, &Target::WholeObject, &p, &RunContext::none())
                    .unwrap();
                let preview =
                    index.preview(max.parse().unwrap(), rel.parse().unwrap(), keep == "true");
                assert_eq!(
                    (preview.pieces, preview.triangles),
                    (
                        out.report["removed_components"].as_u64().unwrap(),
                        out.report["removed_triangles"].as_u64().unwrap()
                    ),
                    "max {max}, {rel}%, keep largest {keep}"
                );
                assert_eq!(preview, removal_preview(&sizes(&mesh), &p).unwrap());
            }
        }
    }
}
