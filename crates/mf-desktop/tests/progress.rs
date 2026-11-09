#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Live progress, Cancel and the frame-time gate on a 5M-triangle mesh
//! (MF-RT-001, MF-RT-002, MF-RT-003).
//!
//! The 5M fixture is generated in memory, never written to disk. The tests
//! hold a lock so they never run (or time) each other, and so only one 5M
//! document is in memory at a time.

use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use eframe::{egui, egui_wgpu};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use mf_desktop::MeshFixerApp;
use mf_fixtures::{DamageSpec, damaged_sphere};
use mf_geometry::{ContentHash, Mesh};
use mf_io::{Format, ImportedModel, SourceInfo, SourcePrecision};
use mf_metrics::{GpuSummary, Percentiles};

const BIG: usize = 5_000_000;
/// MF-RT-001: no UI frame over 50 ms while fill_holes runs on 5M triangles.
const FRAME_BUDGET: Duration = Duration::from_millis(50);

fn heavy() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn big_mesh() -> Mesh {
    static MESH: OnceLock<Mesh> = OnceLock::new();
    MESH.get_or_init(|| damaged_sphere(&DamageSpec::for_triangles(BIG, 11)))
        .clone()
}

fn big_model() -> ImportedModel {
    ImportedModel {
        objects: vec![mf_io::threemf::ModelObject {
            name: "big".into(),
            mesh: big_mesh(),
            transform: mf_io::threemf::IDENTITY,
        }],
        unit: mf_io::threemf::Unit::Millimeter,
        source: SourceInfo {
            path: "big.stl".into(),
            content_hash: ContentHash([0; 32]),
            format: Format::StlBinary,
            precision: SourcePrecision::F32,
        },
        warnings: vec![],
    }
}

/// The app with the 5M sphere open and analysed, on a device with GPU
/// timestamps where the adapter has them, and the render state it runs on.
fn opened() -> (Harness<'static, MeshFixerApp>, egui_wgpu::RenderState) {
    let mut setup = egui_kittest::wgpu::default_wgpu_setup();
    if let egui_wgpu::WgpuSetup::CreateNew(create) = &mut setup {
        create.device_descriptor = Arc::new(mf_desktop::gpu::device_descriptor);
    }
    let render_state =
        egui_kittest::wgpu::create_render_state(setup, egui_wgpu::RendererOptions::PREDICTABLE);
    let mut harness = Harness::builder()
        .with_size((1440.0, 900.0))
        .renderer(egui_kittest::wgpu::WgpuTestRenderer::from_render_state(
            render_state.clone(),
        ))
        .build_eframe(|cc| MeshFixerApp::new(cc, None).unwrap());
    harness.state_mut().import_model(big_model());
    wait_until(&mut harness, "5M analysis", Duration::from_secs(600), |a| {
        a.current_analysis().is_some() && a.viewport.frames_rendered > 0
    });
    (harness, render_state)
}

/// Paints frames the way eframe does (tessellate, upload, draw egui into
/// the frame, submit), into a frame texture kept from frame to frame, then
/// waits for the GPU to finish everything submitted: the 3D view and egui.
/// Headless, there is no swapchain to present to; this stands in for it.
struct Painter {
    render_state: egui_wgpu::RenderState,
    frame: wgpu::TextureView,
    size: [u32; 2],
}

impl Painter {
    fn new(render_state: egui_wgpu::RenderState, size: [u32; 2]) -> Painter {
        let texture = render_state
            .device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("test frame"),
                size: wgpu::Extent3d {
                    width: size[0],
                    height: size[1],
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: render_state.target_format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
        Painter {
            frame: texture.create_view(&Default::default()),
            render_state,
            size,
        }
    }

    fn paint(&self, harness: &Harness<'_, MeshFixerApp>) {
        let rs = &self.render_state;
        let ppp = harness.ctx.pixels_per_point();
        let jobs = harness.ctx.tessellate(harness.output().shapes.clone(), ppp);
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: self.size,
            pixels_per_point: ppp,
        };
        let mut renderer = rs.renderer.write();
        let mut encoder = rs.device.create_command_encoder(&Default::default());
        let uploads = renderer.update_buffers(&rs.device, &rs.queue, &mut encoder, &jobs, &screen);
        {
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("test egui"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &self.frame,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                        depth_slice: None,
                    })],
                    ..Default::default()
                })
                .forget_lifetime();
            renderer.render(&mut pass, &jobs, &screen);
        }
        rs.queue
            .submit(uploads.into_iter().chain(std::iter::once(encoder.finish())));
        rs.device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(Duration::from_secs(10)),
            })
            .unwrap();
    }
}

fn wait_until(
    harness: &mut Harness<'_, MeshFixerApp>,
    what: &str,
    timeout: Duration,
    cond: impl Fn(&MeshFixerApp) -> bool,
) {
    let start = Instant::now();
    while !cond(harness.state()) {
        assert!(start.elapsed() < timeout, "timed out waiting for: {what}");
        harness.step();
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn start_fill(harness: &mut Harness<'_, MeshFixerApp>) {
    harness.get_by_label("Fill all holes").click();
    harness.run_steps(2);
    harness.get_by_label("Apply").click();
    harness.step();
    assert!(harness.state().busy.is_some(), "the fill didn't start");
}

#[test]
fn cancel_button_stops_fill_and_document_is_unchanged() {
    if !mf_render::offscreen::gpu_available() {
        mf_fixtures::skip::gpu_unavailable("cancel_button_stops_fill_and_document_is_unchanged");
        return;
    }
    let _heavy = heavy();
    let (mut harness, _) = opened();
    let app = harness.state();
    let revision = app.session.current();
    let labels = app.session.history_labels();
    let mesh = app.active_mesh().unwrap().clone();

    start_fill(&mut harness);
    // Wait until the fill reports progress: it is really running.
    wait_until(
        &mut harness,
        "fill progress",
        Duration::from_secs(60),
        |a| {
            assert!(
                a.busy.is_some(),
                "the fill finished before it could be cancelled"
            );
            a.job_progress().is_some_and(|(stage, _)| !stage.is_empty())
        },
    );
    harness.step();
    let (stage, fraction) = harness.state().job_progress().unwrap();
    // The status bar shows the stage and a percentage while it runs.
    harness.get_by_label_contains("%");
    harness.get_by_label("Cancel").click();
    let cancelled = Instant::now();
    wait_until(&mut harness, "cancel", Duration::from_secs(10), |a| {
        a.busy.is_none()
    });
    let took = cancelled.elapsed();
    harness.run_steps(2);
    eprintln!("cancelled at {stage} {fraction:.3}; idle again after {took:?}");

    let app = harness.state();
    assert_eq!(app.status.as_deref(), Some("Cancelled: document unchanged"));
    assert!(app.error.is_none(), "{:?}", app.error);
    assert_eq!(app.session.current(), revision);
    assert_eq!(app.session.history_labels(), labels);
    assert!(!app.session.can_redo(), "a cancelled fill left a revision");
    assert!(Mesh::ptr_eq(app.active_mesh().unwrap(), &mesh));
    harness.get_by_label("Cancelled: document unchanged");
    assert!(
        harness.query_by_label("Cancel").is_none(),
        "Cancel is only offered while a job runs"
    );
}

/// Esc stops a running job before it does anything else: the selection
/// stays, and only a second Esc clears it.
#[test]
fn escape_cancels_the_job_first_then_clears_the_selection() {
    if !mf_render::offscreen::gpu_available() {
        mf_fixtures::skip::gpu_unavailable(
            "escape_cancels_the_job_first_then_clears_the_selection",
        );
        return;
    }
    let _heavy = heavy();
    let (mut harness, _) = opened();
    let app = harness.state_mut();
    let object = app.active.unwrap();
    let revision = app.session.current();
    // A few faces selected: Properties still offers the whole-object tools.
    app.selection = Some(mf_selection::Selection {
        object,
        revision,
        kind: mf_selection::SelectionKind::Faces,
        ids: vec![0, 1, 2],
    });
    harness.run_steps(2);
    start_fill(&mut harness);
    wait_until(
        &mut harness,
        "fill progress",
        Duration::from_secs(60),
        |a| {
            assert!(a.busy.is_some(), "the fill finished before Esc");
            a.job_progress().is_some_and(|(stage, _)| !stage.is_empty())
        },
    );
    harness.key_press(egui::Key::Escape);
    wait_until(&mut harness, "cancel", Duration::from_secs(10), |a| {
        a.busy.is_none()
    });
    let app = harness.state();
    assert_eq!(app.status.as_deref(), Some("Cancelled: document unchanged"));
    assert_eq!(app.session.current(), revision);
    assert!(
        app.selection.is_some(),
        "Esc cancelled the job, not the selection"
    );
    harness.key_press(egui::Key::Escape);
    harness.run_steps(2);
    assert!(
        harness.state().selection.is_none(),
        "a second Esc clears it"
    );
}

fn summary(name: &str, p: Option<Percentiles>) -> String {
    match p {
        Some(p) => format!(
            "{name}: p50 {:.1} ms, p95 {:.1} ms, p99 {:.1} ms, max {:.1} ms ({} frames)",
            ms(p.p50()),
            ms(p.p95()),
            ms(p.p99()),
            ms(p.max()),
            p.count()
        ),
        None => format!("{name}: no samples"),
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// One whole frame with the camera moved, so the 3D view is redrawn.
fn orbit_frame(harness: &mut Harness<'_, MeshFixerApp>, painter: &Painter) -> Duration {
    let start = Instant::now();
    if let Some(cam) = harness.state_mut().camera.as_mut() {
        cam.orbit(0.01, 0.0);
    }
    harness.step();
    painter.paint(harness);
    start.elapsed()
}

/// MF-RT-001's acceptance test. A frame here is the whole frame: the app's
/// update (with the 3D view's encode and submit), egui's paint, and waiting
/// for the GPU to finish it all (see [`Painter`]). The camera orbits every
/// frame so the 5M mesh is redrawn every frame.
#[test]
fn ui_frames_stay_under_50ms_while_fill_holes_runs_on_5m() {
    if !mf_render::offscreen::gpu_available() {
        mf_fixtures::skip::gpu_unavailable("ui_frames_stay_under_50ms_while_fill_holes_runs_on_5m");
        return;
    }
    let _heavy = heavy();
    let (mut harness, render_state) = opened();
    let painter = Painter::new(render_state, [1440, 900]);
    // Warm up: the mesh is on the GPU and egui's fonts are cached.
    for _ in 0..5 {
        orbit_frame(&mut harness, &painter);
    }
    let mut idle = Vec::new();
    for _ in 0..20 {
        idle.push(orbit_frame(&mut harness, &painter));
    }
    let labels = harness.state().session.history_labels().len();
    let rendered = harness.state().viewport.frames_rendered;

    harness.get_by_label("Fill all holes").click();
    harness.run_steps(2);
    // Every frame from the one that handles the Apply click to the one
    // that commits the result is timed.
    harness.get_by_label("Apply").click();
    let mut frames: Vec<Duration> = Vec::new();
    let mut worst = (Duration::ZERO, String::new());
    let mut started = false;
    loop {
        let took = orbit_frame(&mut harness, &painter);
        let app = harness.state();
        if took > worst.0 {
            let at = app
                .job_progress()
                .map_or("no job".to_owned(), |(s, f)| format!("{s} {f:.3}"));
            worst = (took, at);
        }
        frames.push(took);
        started |= app.busy.is_some();
        if started && app.busy.is_none() {
            break;
        }
        assert!(started || frames.len() < 10, "the fill didn't start");
        assert!(frames.len() < 100_000, "the fill never finished");
    }
    // Not gated here: after the commit the new revision is analysed and
    // its vertices uploaded to the GPU on the UI thread (a later task
    // splits that upload), reported so the hitch is visible.
    let mut after = Vec::new();
    let settle = Instant::now();
    while harness.state().is_analyzing() && settle.elapsed() < Duration::from_secs(120) {
        after.push(orbit_frame(&mut harness, &painter));
    }
    for _ in 0..5 {
        after.push(orbit_frame(&mut harness, &painter));
    }
    eprintln!(
        "{}",
        summary(
            "after the fill (re-analysis, GPU upload; not gated)",
            Percentiles::of(after)
        )
    );
    let app = harness.state();
    assert!(app.error.is_none(), "{:?}", app.error);
    assert_eq!(
        app.session.history_labels().len(),
        labels + 1,
        "the fill committed"
    );
    assert!(
        app.viewport.frames_rendered >= rendered + frames.len() as u64 / 2,
        "the 3D view was redrawn as the camera moved"
    );

    let whole = Percentiles::of(frames.clone());
    eprintln!(
        "{}",
        summary("idle whole frame (before the fill)", Percentiles::of(idle))
    );
    eprintln!("{} frames while fill_holes ran", frames.len());
    eprintln!("{}", summary("whole frame", whole.clone()));
    eprintln!("worst frame {:.1} ms (at {})", ms(worst.0), worst.1);
    let times = &app.frame_times;
    eprintln!(
        "{}",
        summary("app CPU (update + encode/submit)", times.cpu())
    );
    eprintln!("{}", summary("app frame interval", times.interval()));
    match times.gpu() {
        GpuSummary::Measured(p) => eprintln!(
            "{} ({} samples unusable)",
            summary("GPU (3D view)", Some(p)),
            app.viewport.gpu_samples_failed().unwrap()
        ),
        GpuSummary::Unsupported => eprintln!("GPU (3D view): timestamps unsupported"),
        // Possible on Metal, whose heavy-pass timestamps come back out of
        // order (see mf-render's timer.rs); the gate below doesn't use them.
        GpuSummary::NoSamples => eprintln!(
            "GPU (3D view): no usable samples ({} unusable)",
            app.viewport.gpu_samples_failed().unwrap()
        ),
    }
    assert!(
        worst.0 <= FRAME_BUDGET,
        "worst frame {:.1} ms at {} (budget {} ms)",
        ms(worst.0),
        worst.1,
        FRAME_BUDGET.as_millis()
    );
    assert!(
        frames.len() >= 10,
        "too few frames to judge: {}",
        frames.len()
    );
}
