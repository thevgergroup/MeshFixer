#![allow(clippy::unwrap_used)] // tests may panic on failure
//! README screenshots: opens the Stanford bunny and steps through the main
//! tools, saving one 1440×900 frame per step to `$MF_SCREENSHOTS`.
//!
//! On demand only (ignored by default):
//! `MF_SCREENSHOTS=docs/images cargo test -p mf-desktop --test readme_screenshots -- --ignored`

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use mf_desktop::{MeshFixerApp, Settings};
use mf_geometry::ContentHash;
use mf_io::threemf::{ModelObject, Unit};
use mf_io::{Format, ImportedModel, SourceInfo, SourcePrecision};

const WINDOW: (f32, f32) = (1440.0, 900.0);

fn wait_until(h: &mut Harness<'_, MeshFixerApp>, what: &str, cond: impl Fn(&MeshFixerApp) -> bool) {
    let start = Instant::now();
    while !cond(h.state()) {
        assert!(
            start.elapsed() < Duration::from_secs(120),
            "timed out waiting for: {what} (error {:?}, refusal {:?})",
            h.state().error,
            h.state().refusal,
        );
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn settle(h: &mut Harness<'_, MeshFixerApp>) {
    let start = Instant::now();
    loop {
        assert!(start.elapsed() < Duration::from_secs(120), "never settled");
        h.step();
        if !h.state().background_work_in_flight() && !h.ctx.has_requested_repaint() {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    h.run_steps(3);
}

fn shot(h: &mut Harness<'_, MeshFixerApp>, dir: &Path, name: &str) {
    settle(h);
    h.event(egui::Event::PointerGone);
    h.run_steps(2);
    let path = dir.join(format!("{name}.png"));
    h.render().unwrap().save(&path).unwrap();
    println!("saved {}", path.display());
}

fn rail(h: &mut Harness<'_, MeshFixerApp>, tool: &str) {
    h.query_all_by_label(tool)
        .next()
        .unwrap_or_else(|| panic!("no {tool:?} on the rail"))
        .click();
    settle(h);
}

fn bunny_path() -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/real/stanford-bunny.obj");
    assert!(
        p.exists(),
        "README shots need fixtures/real/stanford-bunny.obj"
    );
    p
}

fn opened_bunny(config: &Path) -> Harness<'static, MeshFixerApp> {
    let model = mf_io::read_model(&bunny_path()).unwrap();
    let bunny = model.objects.into_iter().next().unwrap().mesh;
    // The scan is y-up; stand it upright (+90° about X: y becomes z) and
    // rest it on the plate.
    let min_y = bunny
        .vertices()
        .map(|(_, p)| p[1])
        .fold(f64::INFINITY, f64::min);
    let settings = Settings {
        config_dir: Some(config.to_owned()),
        ..Settings::default()
    };
    let mut h = Harness::builder()
        .with_size(WINDOW)
        .with_step_dt(1.0 / 60.0)
        .wgpu()
        .build_eframe(move |cc| MeshFixerApp::with_settings(cc, None, settings).unwrap());
    h.state_mut().import_model(ImportedModel {
        metadata: Default::default(),
        objects: vec![ModelObject {
            metadata: Default::default(),
            name: "Stanford bunny".into(),
            mesh: bunny,
            transform: [
                1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, -1.0, 0.0, 0.0, 0.0, -min_y,
            ],
        }],
        unit: Some(Unit::Meter),
        source: SourceInfo {
            path: "stanford-bunny.obj".into(),
            content_hash: ContentHash([0; 32]),
            format: Format::Obj,
            precision: SourcePrecision::F32,
            exporter_hint: None,
        },
        warnings: Vec::new(),
    });
    wait_until(&mut h, "analysis and picking", |a| {
        a.current_analysis().is_some() && a.picking_ready() && a.camera.is_some()
    });
    settle(&mut h);
    h
}

#[test]
#[ignore = "on demand: README screenshots"]
fn readme_screenshots() {
    let Some(dir) = std::env::var_os("MF_SCREENSHOTS").map(PathBuf::from) else {
        panic!("set MF_SCREENSHOTS to the output directory");
    };
    std::fs::create_dir_all(&dir).unwrap();
    let config = tempfile::tempdir().unwrap();

    // The import dialog: STL/OBJ carry no units, so MeshFixer offers the
    // one that fits the plate.
    {
        let settings = Settings {
            unit_prompt: true,
            config_dir: Some(config.path().to_owned()),
            ..Settings::default()
        };
        let path = bunny_path();
        let mut h = Harness::builder()
            .with_size(WINDOW)
            .with_step_dt(1.0 / 60.0)
            .wgpu()
            .build_eframe(move |cc| {
                MeshFixerApp::with_settings(cc, Some(path.clone()), settings).unwrap()
            });
        wait_until(&mut h, "the unit dialog", |a| a.unit_prompt().is_some());
        shot(&mut h, &dir, "import-units");
    }

    let mut h = opened_bunny(config.path());
    shot(&mut h, &dir, "overview");
    rail(&mut h, "Inspect");
    shot(&mut h, &dir, "inspect");
    h.key_press(egui::Key::Escape);
    settle(&mut h);

    // Repair: the scan's open base and holes, then filled.
    rail(&mut h, "Repair");
    shot(&mut h, &dir, "repair");
    h.get_by_label("Fill all holes").click();
    settle(&mut h);
    // Fill every opening, the bunny's 80-edge base included.
    h.state_mut()
        .params_for("mesh.fill_holes")
        .insert("max_loop_edges".into(), mf_spec::ParamValue::Int(200));
    settle(&mut h);
    shot(&mut h, &dir, "repair-preview");
    let rev = h.state().session.current();
    h.get_by_label("Apply").click();
    wait_until(&mut h, "the fill", |a| {
        a.session.current() != rev && a.busy.is_none() && a.hole_view().is_some()
    });
    shot(&mut h, &dir, "repaired");
    h.key_press(egui::Key::Escape);
    settle(&mut h);

    for (tool, name) in [
        ("Split", "split"),
        ("Hollow", "hollow"),
        ("Sculpt", "sculpt"),
        ("Parts", "parts"),
        ("Transform", "transform"),
    ] {
        rail(&mut h, tool);
        shot(&mut h, &dir, name);
        h.key_press(egui::Key::Escape);
        settle(&mut h);
    }
}
