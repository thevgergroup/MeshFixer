#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Headless end-to-end GUI test (M1 demo): open a damaged mesh, inspect it,
//! apply a spec-generated operation, undo and redo.

use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use mf_desktop::MeshFixerApp;
use mf_fixtures::{DamageSpec, damaged_sphere};

fn wait_until(
    harness: &mut Harness<'_, MeshFixerApp>,
    what: &str,
    cond: impl Fn(&MeshFixerApp) -> bool,
) {
    let start = Instant::now();
    while !cond(harness.state()) {
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "timed out waiting for: {what}"
        );
        harness.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    harness.run_steps(3);
}

fn components(app: &MeshFixerApp) -> Option<usize> {
    app.current_analysis().map(|a| a.diagnostics.components)
}

fn save(harness: &mut Harness<'_, MeshFixerApp>, name: &str) {
    if let Ok(dir) = std::env::var("MF_ARTIFACTS") {
        let img = harness.render().unwrap();
        img.save(std::path::Path::new(&dir).join(format!("{name}.png")))
            .unwrap();
    }
}

#[test]
fn open_inspect_apply_undo_redo() {
    if !mf_render::offscreen::gpu_available() {
        mf_fixtures::skip::gpu_unavailable("open_inspect_apply_undo_redo");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("damaged.obj");
    let spec = DamageSpec {
        rings: 48,
        segments: 96,
        holes: 4,
        islands: 20,
        unwelded: 0,
        seed: 2,
    };
    mf_io::write_path(&damaged_sphere(&spec), &path, mf_io::Format::Obj).unwrap();

    let mut harness = Harness::builder()
        .with_size((1400.0, 900.0))
        .wgpu()
        .build_eframe(|cc| MeshFixerApp::new(cc, Some(path.clone())).unwrap());

    wait_until(&mut harness, "import + analysis", |a| {
        components(a).is_some()
    });
    // Settle once so egui_kittest's accessibility check always runs (it only
    // runs when the UI settles, which otherwise depends on timing).
    harness.run();
    let before = components(harness.state()).unwrap();
    assert!(before >= 21, "sphere + 20 islands, got {before}");
    assert!(harness.state().viewport.frames_rendered >= 1);
    save(&mut harness, "01-opened");

    // Idle frames don't re-render the 3D view.
    let rendered = harness.state().viewport.frames_rendered;
    harness.run_steps(5);
    assert_eq!(harness.state().viewport.frames_rendered, rendered);

    harness.get_by_label("Apply").click();
    wait_until(&mut harness, "operation + re-analysis", |a| {
        a.session.history_labels().len() == 3 && components(a).is_some_and(|c| c < before)
    });
    let after = components(harness.state()).unwrap();
    assert_eq!(after, 1, "every component under 100 triangles is removed");
    assert!(
        harness.state().error.is_none(),
        "{:?}",
        harness.state().error
    );
    save(&mut harness, "02-applied");

    harness.get_by_label("Undo").click();
    harness.step();
    // The pre-apply revision is cached, so its diagnostics are correct at once,
    // never the post-apply numbers carried over.
    assert_eq!(components(harness.state()), Some(before));
    assert!(!harness.state().is_analyzing());
    assert!(harness.state().session.can_redo());
    save(&mut harness, "03-undone");

    harness.get_by_label("Redo").click();
    wait_until(&mut harness, "redo", |a| components(a) == Some(after));

    // Branch: undo, then apply with a different parameter. The first branch
    // is retained in the revision DAG (MF-HIST-001).
    harness.get_by_label("Undo").click();
    wait_until(&mut harness, "undo before branching", |a| {
        components(a) == Some(before)
    });
    let base = harness.state().session.current();
    harness
        .state_mut()
        .params_for("mesh.remove_small_components")
        .insert("max_triangles".into(), mf_spec::ParamValue::Int(6));
    harness.get_by_label("Apply").click();
    wait_until(&mut harness, "branch apply", |a| {
        a.session.current() != base && components(a).is_some()
    });
    assert_eq!(
        harness.state().session.node(base).unwrap().children.len(),
        2
    );
    save(&mut harness, "04-branched");

    // Save the command log from the app, then replay it headlessly.
    let log = dir.path().join("session.mfx.log");
    harness.state_mut().save_log_to(&log).unwrap();
    let report = mf_command::replay(&log).unwrap();
    assert!(report.all_verified(), "{:?}", report.results);
}
