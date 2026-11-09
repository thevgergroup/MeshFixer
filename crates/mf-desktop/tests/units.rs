#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Units for unitless files (MF-ISSUE-045): STL and OBJ don't say what
//! their numbers measure, so opening one asks. The dialog shows the
//! model's size read in millimetres, centimetres, metres and inches and
//! preselects the one that makes it between 10 mm and 2 m (else
//! millimetres); the document records the choice and 3MF export writes it.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use mf_desktop::{MeshFixerApp, Settings};
use mf_io::threemf::Unit;

const WINDOW: (f32, f32) = (1440.0, 900.0);

macro_rules! require_gpu {
    ($name:literal) => {
        if !mf_render::offscreen::gpu_available() {
            mf_fixtures::skip::gpu_unavailable($name);
            return;
        }
    };
}

fn wait_until(
    h: &mut Harness<'_, MeshFixerApp>,
    what: &str,
    timeout: Duration,
    cond: impl Fn(&MeshFixerApp) -> bool,
) {
    let start = Instant::now();
    while !cond(h.state()) {
        assert!(start.elapsed() < timeout, "timed out waiting for: {what}");
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    h.run_steps(3);
}

/// A closed 0.5 × 0.4 × 0.3 box (an Archaeopteryx-sized scan, in metres),
/// as binary STL.
fn metre_scale_box(dir: &Path) -> PathBuf {
    let path = dir.join("scan.stl");
    let (positions, triangles) = mf_fixtures::cube(1.0).to_arrays();
    let positions = positions
        .iter()
        .map(|p| [p[0] * 0.5, p[1] * 0.4, p[2] * 0.3])
        .collect();
    let mesh = mf_geometry::Mesh::new(positions, triangles).unwrap();
    mf_io::write_path(&mesh, &path, mf_io::Format::StlBinary).unwrap();
    path
}

/// The app as launched by a person (it asks for the units of a unitless
/// file), opening `path`.
fn opening(path: PathBuf) -> Harness<'static, MeshFixerApp> {
    Harness::builder()
        .with_size(WINDOW)
        .with_step_dt(1.0 / 60.0)
        .wgpu()
        .build_eframe(move |cc| {
            let settings = Settings {
                unit_prompt: true,
                ..Settings::default()
            };
            MeshFixerApp::with_settings(cc, Some(path.clone()), settings).unwrap()
        })
}

/// Opens `path`, waits for the unit dialog, checks it preselected metres
/// (its size row reads in metres), and imports.
fn import_at_the_suggested_unit(h: &mut Harness<'_, MeshFixerApp>, timeout: Duration) {
    wait_until(h, "the unit dialog", timeout, |a| a.unit_prompt().is_some());
    h.run();
    let prompt = h.state().unit_prompt().unwrap();
    assert_eq!(prompt.choice, Unit::Meter, "{prompt:?}");
    // Each unit's row says how big the model would be.
    for unit in ["Millimetres", "Centimetres", "Metres", "Inches"] {
        let row = h
            .get_all_by_role(egui::accesskit::Role::RadioButton)
            .find(|n| {
                n.accesskit_node()
                    .label()
                    .is_some_and(|l| l.starts_with(unit))
            })
            .unwrap_or_else(|| panic!("a {unit} row"));
        let selected = row.accesskit_node().toggled() == Some(egui::accesskit::Toggled::True);
        assert_eq!(
            selected,
            unit == "Metres",
            "{:?}",
            row.accesskit_node().label()
        );
    }
    h.get_by_label("Import").click();
    wait_until(h, "import + analysis", timeout, |a| {
        a.unit_prompt().is_none() && a.current_analysis().is_some()
    });
}

#[test]
fn unit_prompt_preselects_metres_for_the_archaeopteryx_scale() {
    require_gpu!("unit_prompt_preselects_metres_for_the_archaeopteryx_scale");
    let dir = tempfile::tempdir().unwrap();
    let mut h = opening(metre_scale_box(dir.path()));
    wait_until(&mut h, "the unit dialog", Duration::from_secs(30), |a| {
        a.unit_prompt().is_some()
    });
    h.run();
    // The size in each unit: 0.5 × 0.4 × 0.3 (the numbers don't change,
    // only what they mean), with millimetres alongside.
    h.get_by_label("Metres · 0.500 × 0.400 × 0.300 m (500 × 400 × 300 mm)");
    h.get_by_label("Millimetres · 0.500 × 0.400 × 0.300 mm");
    import_at_the_suggested_unit(&mut h, Duration::from_secs(30));
    assert_eq!(h.state().session.document().units(), Unit::Meter);
    // Measurements read in the chosen unit.
    h.get_by_label("0.500 × 0.400 × 0.300 m");
}

/// Esc (or Cancel) in the dialog imports nothing.
#[test]
fn cancelling_the_unit_prompt_imports_nothing() {
    require_gpu!("cancelling_the_unit_prompt_imports_nothing");
    let dir = tempfile::tempdir().unwrap();
    let mut h = opening(metre_scale_box(dir.path()));
    wait_until(&mut h, "the unit dialog", Duration::from_secs(30), |a| {
        a.unit_prompt().is_some()
    });
    h.run();
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(h.state().unit_prompt().is_none());
    assert!(h.state().session.document().objects().is_empty());
    assert_eq!(
        h.state().status.as_deref(),
        Some("Import cancelled: nothing opened")
    );
}

/// The real Archaeopteryx scan (0.495 × 0.410 m) imports at metre scale.
#[test]
fn unit_prompt_imports_the_real_scan_at_metre_scale() {
    require_gpu!("unit_prompt_imports_the_real_scan_at_metre_scale");
    let Some(path) = mf_fixtures::real::path(
        "archaeopteryx.obj",
        "unit_prompt_imports_the_real_scan_at_metre_scale",
    ) else {
        return;
    };
    let mut h = opening(path);
    import_at_the_suggested_unit(&mut h, Duration::from_secs(300));
    assert_eq!(h.state().session.document().units(), Unit::Meter);
}

#[test]
fn exported_3mf_carries_the_chosen_unit() {
    require_gpu!("exported_3mf_carries_the_chosen_unit");
    let dir = tempfile::tempdir().unwrap();
    let mut h = opening(metre_scale_box(dir.path()));
    import_at_the_suggested_unit(&mut h, Duration::from_secs(30));
    let out = dir.path().join("out.3mf");
    h.state_mut().export_to(out.clone());
    wait_until(&mut h, "export", Duration::from_secs(30), |a| {
        a.busy.is_none()
    });
    assert_eq!(h.state().error, None);
    let back = mf_io::read_model(&out).unwrap();
    assert_eq!(back.unit, Unit::Meter);
}

/// Without the prompt (tests, scripted use), a unitless file reads as
/// millimetres and says so, never silently.
#[test]
fn without_the_prompt_a_unitless_file_reads_as_millimetres_and_says_so() {
    require_gpu!("without_the_prompt_a_unitless_file_reads_as_millimetres_and_says_so");
    let dir = tempfile::tempdir().unwrap();
    let path = metre_scale_box(dir.path());
    let mut h = Harness::builder()
        .with_size(WINDOW)
        .wgpu()
        .build_eframe(move |cc| MeshFixerApp::new(cc, Some(path.clone())).unwrap());
    wait_until(&mut h, "import", Duration::from_secs(30), |a| {
        a.current_analysis().is_some()
    });
    assert_eq!(h.state().session.document().units(), Unit::Millimeter);
    assert!(
        h.state()
            .warnings
            .iter()
            .any(|w| w.contains("no units") && w.contains("millimetres")),
        "{:?}",
        h.state().warnings
    );
}
