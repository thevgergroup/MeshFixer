#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The pseudo-locale (UI plan §10): with it on, every catalogued string is
//! accented, about 40% longer and bracketed in [[…]], so text not yet
//! converted to message ids, and layouts that break with longer text, show
//! in frames. Its own test binary: the switch is process-wide.
//!
//! With `MF_ARTIFACTS=<dir>` set, frames go to
//! `<dir>/design/pseudo-<scene>-<w>x<h>.png`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use mf_desktop::{MeshFixerApp, Settings};
use mf_geometry::ContentHash;
use mf_io::threemf::{IDENTITY, ModelObject, Unit};
use mf_io::{Format, ImportedModel, SourceInfo, SourcePrecision};

fn still(h: &mut Harness<'_, MeshFixerApp>, scene: &str, size: (f32, f32)) {
    let Some(dir) = std::env::var_os("MF_ARTIFACTS") else {
        return;
    };
    let dir = PathBuf::from(dir).join("design");
    std::fs::create_dir_all(&dir).unwrap();
    let name = format!("pseudo-{scene}-{}x{}.png", size.0 as u32, size.1 as u32);
    h.render().unwrap().save(dir.join(name)).unwrap();
}

fn settle(h: &mut Harness<'_, MeshFixerApp>) {
    let start = Instant::now();
    loop {
        assert!(start.elapsed() < Duration::from_secs(30), "never settled");
        h.step();
        if !h.state().background_work_in_flight() && !h.ctx.has_requested_repaint() {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    h.run_steps(3);
}

#[test]
fn the_pseudo_locale_brackets_catalogued_strings() {
    if !mf_render::offscreen::gpu_available() {
        mf_fixtures::skip::gpu_unavailable("the_pseudo_locale_brackets_catalogued_strings");
        return;
    }
    mf_desktop::i18n::set_pseudo(true);
    for size in [(1440.0, 900.0), (1280.0, 720.0)] {
        let mut h = Harness::builder()
            .with_size(size)
            .with_step_dt(1.0 / 60.0)
            .wgpu()
            .build_eframe(move |cc| {
                MeshFixerApp::with_settings(cc, None, Settings::default()).unwrap()
            });
        let mesh = mf_fixtures::merge(&[
            mf_fixtures::plate_with_defects(),
            mf_fixtures::translated(&mf_fixtures::cube(2.0), [60.0, 0.0, 0.0]),
        ]);
        h.state_mut().import_model(ImportedModel {
            metadata: Default::default(),
            objects: vec![ModelObject {
                metadata: Default::default(),
                name: "plate".into(),
                mesh,
                transform: IDENTITY,
            }],
            unit: Some(Unit::Millimeter),
            source: SourceInfo {
                path: "plate.3mf".into(),
                content_hash: ContentHash([0; 32]),
                format: Format::ThreeMf,
                precision: SourcePrecision::F32,
                exporter_hint: None,
            },
            warnings: Vec::new(),
        });
        let start = Instant::now();
        while h.state().hole_view().is_none() || h.state().camera.is_none() {
            assert!(start.elapsed() < Duration::from_secs(60));
            h.step();
            std::thread::sleep(Duration::from_millis(5));
        }
        h.event(egui::Event::PointerGone);
        settle(&mut h);
        // The rail's tools read in the pseudo-locale.
        let transform = mf_desktop::i18n::tr("tool.transform.name", &[]);
        assert!(transform.starts_with("[[") && transform.ends_with("]]"));
        h.get_by_label(&transform);
        still(&mut h, "no-tool", size);
        h.get_by_label(&mf_desktop::i18n::tr("tool.repair.name", &[]))
            .click();
        settle(&mut h);
        still(&mut h, "repair", size);
        h.key_press(egui::Key::T);
        settle(&mut h);
        still(&mut h, "transform", size);
    }
    mf_desktop::i18n::set_pseudo(false);
}
