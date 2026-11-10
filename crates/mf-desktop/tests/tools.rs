#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The tool framework (M4 final UX review I8): Properties offers a curated,
//! grouped list of whole-object tools (never the raw spec registry), and
//! the legend is collapsible, so the left panel has room for more.

use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use mf_desktop::MeshFixerApp;

fn wait_until(h: &mut Harness<'_, MeshFixerApp>, what: &str, cond: impl Fn(&MeshFixerApp) -> bool) {
    let start = Instant::now();
    while !cond(h.state()) {
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "timed out waiting for: {what}"
        );
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    h.run_steps(3);
}

macro_rules! require_gpu {
    ($name:literal) => {
        if !mf_render::offscreen::gpu_available() {
            mf_fixtures::skip::gpu_unavailable($name);
            return;
        }
    };
}

fn open_cube() -> (tempfile::TempDir, Harness<'static, MeshFixerApp>) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cube.obj");
    mf_io::write_path(
        &mf_fixtures::cube(10.0),
        &path,
        mf_io::Format::Obj,
        mf_io::threemf::Unit::Millimeter,
    )
    .unwrap();
    let mut h = Harness::builder()
        .with_size((1440.0, 900.0))
        .wgpu()
        .build_eframe(move |cc| MeshFixerApp::new(cc, Some(path.clone())).unwrap());
    wait_until(&mut h, "import + analysis", |a| {
        a.current_analysis().is_some()
    });
    (dir, h)
}

/// The whole-object tools are the catalogue's, in its groups and order:
/// no object operation (delete, drop to plate, set transform) shows up as
/// a raw "parameters + Apply" button.
#[test]
fn whole_object_tools_are_curated_and_grouped() {
    require_gpu!("whole_object_tools_are_curated_and_grouped");
    let (_dir, mut h) = open_cube();
    h.run();
    let ids: Vec<String> = h.state().tools().into_iter().map(|t| t.op_id).collect();
    let catalogue: Vec<String> = mf_desktop::tools::CATALOGUE
        .iter()
        .map(|e| e.id.to_owned())
        .collect();
    assert_eq!(ids, catalogue);
    for label in ["Repair", "Housekeeping", "Fill all holes", "Compact"] {
        h.get_by_label(label);
    }
    for raw in [
        "object.set_transform",
        "object.drop_to_plate",
        "object.delete",
        "mesh.inspect",
    ] {
        assert!(h.query_by_label(raw).is_none(), "{raw} is offered raw");
    }
}

/// The legend folds away (its rows go) and comes back.
#[test]
fn the_legend_collapses_to_make_room() {
    require_gpu!("the_legend_collapses_to_make_room");
    let (_dir, mut h) = open_cube();
    h.run();
    h.get_by_label("Recent repairs");
    h.get_by_label("Legend").click();
    h.run();
    assert!(h.query_by_label("Recent repairs").is_none());
    h.get_by_label("Legend").click();
    h.run();
    h.get_by_label("Recent repairs");
}
