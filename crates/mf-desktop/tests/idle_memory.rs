#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The app sitting idle with a model open and a previewing tool open must
//! hold steady: no heap growth, no texture or GPU buffer growth, and no
//! frame-after-frame repaint (owner report: 25 GB after hours idle).
//!
//! Its own binary: the counting allocator is process-wide.

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use mf_desktop::{MeshFixerApp, Settings};
use mf_geometry::{ContentHash, Mesh};
use mf_io::threemf::ModelObject;
use mf_io::{Format, ImportedModel, SourceInfo, SourcePrecision};
use mf_metrics::CountingAlloc;
use mf_units::Unit;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

const WINDOW: (f32, f32) = (1440.0, 1000.0);

fn settings_dir() -> &'static std::path::Path {
    static DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    DIR.get_or_init(|| tempfile::tempdir().unwrap()).path()
}

fn bunny() -> Option<Mesh> {
    let path = mf_fixtures::real::path(
        "stanford-bunny.obj",
        "idle_with_tool_open_memory_is_bounded",
    )?;
    Some(mf_io::read_model(&path).unwrap().objects.remove(0).mesh)
}

fn opened(objects: Vec<(&str, Mesh, [f64; 12])>) -> Harness<'static, MeshFixerApp> {
    let settings = Settings {
        config_dir: Some(settings_dir().to_owned()),
        ..Settings::default()
    };
    let mut h = Harness::builder()
        .with_size(WINDOW)
        .with_step_dt(1.0 / 60.0)
        .wgpu()
        .build_eframe(move |cc| MeshFixerApp::with_settings(cc, None, settings).unwrap());
    h.state_mut().import_model(ImportedModel {
        metadata: Default::default(),
        objects: objects
            .into_iter()
            .map(|(name, mesh, transform)| ModelObject {
                metadata: Default::default(),
                name: name.into(),
                mesh,
                transform,
            })
            .collect(),
        unit: Some(Unit::Meter),
        source: SourceInfo {
            path: "idle.3mf".into(),
            content_hash: ContentHash([0; 32]),
            format: Format::ThreeMf,
            precision: SourcePrecision::F32,
            exporter_hint: None,
        },
        warnings: Vec::new(),
    });
    let start = std::time::Instant::now();
    while !(h.state().current_analysis().is_some() && h.state().picking_ready()) {
        assert!(start.elapsed().as_secs() < 300, "never analysed");
        h.step();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    h.event(egui::Event::PointerGone);
    h
}

fn at(t: [f64; 3]) -> [f64; 12] {
    [
        1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, t[0], t[1], t[2],
    ]
}

#[derive(Debug, Clone, Copy)]
struct Sample {
    heap: u64,
    textures: usize,
    gpu: u64,
}

fn sample(h: &Harness<'_, MeshFixerApp>) -> Sample {
    Sample {
        heap: mf_metrics::live_bytes(),
        textures: h.ctx.tex_manager().read().num_allocated(),
        gpu: h
            .state()
            .object_views()
            .iter()
            .map(|v| v.render.gpu.resident_bytes())
            .sum(),
    }
}

/// Step until nothing asks for an immediate repaint (the tool's preview
/// has come back), at most `max` frames.
fn settle(h: &mut Harness<'_, MeshFixerApp>, max: usize) {
    for _ in 0..max {
        h.step();
        std::thread::sleep(std::time::Duration::from_millis(2));
        if !h.ctx.requested_repaint_last_pass() {
            return;
        }
    }
    panic!(
        "still repainting every frame after {max} frames: {:?}",
        h.ctx.repaint_causes()
    );
}

struct Soak {
    first: Sample,
    mid: Sample,
    last: Sample,
    immediate_repaints: usize,
}

/// The bunny, repaired (or two of them, both selected, for the tools
/// that take two objects), `tool` open, the pointer resting over the view or not;
/// then `frames` frames with no input at all.
fn soak(tool: &'static str, frames: usize, hover: bool) -> Option<Soak> {
    let b = bunny()?;
    let two = matches!(tool, "combine" | "connect" | "align");
    let objects = if two {
        vec![
            ("a", b.clone(), at([0.0, 0.0, 0.0])),
            ("b", b, at([0.1, 0.0, 0.0])),
        ]
    } else {
        vec![("bunny", b, at([0.0, 0.0, 0.0]))]
    };
    let mut h = opened(objects);
    if !two {
        // Repair first (Fill all holes), as Hollow needs a closed mesh:
        // the bunny has open holes in its base.
        h.get_by_label("Repair").click();
        h.run_steps(3);
        h.get_by_label("Fill all holes").click();
        h.run_steps(3);
        let rev = h.state().session.current();
        h.get_by_label("Apply").click();
        let start = std::time::Instant::now();
        while h.state().session.current() == rev
            || h.state().busy.is_some()
            || h.state().current_analysis().is_none()
            || !h.state().picking_ready()
        {
            assert!(start.elapsed().as_secs() < 300, "the repair never finished");
            h.step();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(h.state().error.is_none(), "{:?}", h.state().error);
    }
    if two {
        h.get_by_role_and_label(egui::accesskit::Role::Button, "a")
            .click();
        h.run_steps(3);
        h.get_by_role_and_label(egui::accesskit::Role::Button, "b")
            .click_modifiers(egui::Modifiers::SHIFT);
        h.run_steps(3);
    }
    h.state_mut().open_rail_tool(tool);
    assert_eq!(
        h.state().active_tool,
        tool,
        "{tool} did not open: {:?}",
        h.state().status
    );
    if hover {
        let r = h.state().viewport.last_rect.unwrap();
        h.event(egui::Event::PointerMoved(r.center()));
    }
    if tool == mf_desktop::hollow_tool::HOLLOW {
        // Idle with the hollowed preview showing, not before it's made.
        let id = h.state().active.unwrap();
        let start = std::time::Instant::now();
        while mf_desktop::hollow_tool::preview_render(h.state(), id).is_none() {
            assert!(
                start.elapsed().as_secs() < 300,
                "Hollow's preview never came back"
            );
            h.step();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    settle(&mut h, 600);
    // Warm-up: caches fill, buffers reach their working size, and egui's
    // own one-off allocations (tens of KB, within the first ~10 s of
    // frames, then never again: 600 and 6000 frames grow the same) land.
    h.run_steps(WARM_UP);
    let first = sample(&h);
    let mut immediate_repaints = 0;
    let mut mid = first;
    for i in 0..frames {
        h.step();
        if h.ctx.requested_repaint_last_pass() {
            immediate_repaints += 1;
        }
        if i == frames / 2 {
            mid = sample(&h);
        }
    }
    Some(Soak {
        first,
        mid,
        last: sample(&h),
        immediate_repaints,
    })
}

const WARM_UP: usize = 720;

const TOOLS: [&str; 8] = [
    mf_desktop::hollow_tool::HOLLOW,
    mf_desktop::shape_tools::SPLIT,
    mf_desktop::sculpt_tool::SCULPT,
    mf_desktop::shape_tools::MAKE_SOLID,
    mf_desktop::shape_tools::COMBINE,
    mf_desktop::connect_tool::CONNECT,
    mf_desktop::align_tool::ID,
    mf_desktop::transform_tool::ID,
];

/// Owner report (2026-11-15): "out of application memory", the app said
/// to be at 25 GB while left open. Idle with the bunny (in metres) and
/// each previewing tool open, with and without the pointer resting over
/// the view, the app must not repaint frame after frame, and its heap,
/// egui textures and GPU buffers must stay flat.
#[test]
fn idle_with_tool_open_memory_is_bounded() {
    if !mf_render::offscreen::gpu_available() {
        mf_fixtures::skip::gpu_unavailable("idle_with_tool_open_memory_is_bounded");
        return;
    }
    const FRAMES: usize = 300;
    // A one-off allocation (a lazily built cache) may land in the window;
    // a per-frame leak of even 64 bytes would not fit under this.
    const SECOND_HALF_SLACK: u64 = 8 * 1024;
    const TOTAL_SLACK: u64 = 64 * 1024;
    for (tool, hover) in TOOLS.iter().flat_map(|t| [(*t, false), (*t, true)]) {
        let Some(s) = soak(tool, FRAMES, hover) else {
            return;
        };
        eprintln!(
            "{tool:>11} hover={hover}: immediate repaints {}/{FRAMES}, heap {} -> {} -> {}, \
             textures {} -> {}, GPU {} -> {}",
            s.immediate_repaints,
            s.first.heap,
            s.mid.heap,
            s.last.heap,
            s.first.textures,
            s.last.textures,
            s.first.gpu,
            s.last.gpu,
        );
        // A result arriving may ask for a frame or two; a loop asks for all.
        assert!(
            s.immediate_repaints <= 5,
            "{tool} (hover {hover}) repainted {} of {FRAMES} idle frames",
            s.immediate_repaints
        );
        assert!(
            s.last.heap <= s.mid.heap + SECOND_HALF_SLACK
                && s.last.heap <= s.first.heap + TOTAL_SLACK,
            "{tool} (hover {hover}) heap grew while idle: {} -> {} -> {}",
            s.first.heap,
            s.mid.heap,
            s.last.heap
        );
        assert_eq!(
            s.first.textures, s.last.textures,
            "{tool} (hover {hover}) textures"
        );
        assert_eq!(
            s.first.gpu, s.last.gpu,
            "{tool} (hover {hover}) GPU buffers"
        );
    }
}
