#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Live progress, Cancel and the frame-time gate on a 5M-triangle mesh
//! (MF-RT-001, MF-RT-002, MF-RT-003).
//!
//! The 5M fixture is generated in memory, never written to disk. The tests
//! hold a lock so they never run (or time) each other, and so only one 5M
//! document is in memory at a time.
//!
//! With `MF_ARTIFACTS` set, `cancel_button_stops_fill_and_document_is_unchanged`
//! also writes a mid-run frame (progress bar and percentage visible) there
//! as `progress-mid-run.png` (milestone evidence).

use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use eframe::{egui, egui_wgpu};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use mf_desktop::{MeshFixerApp, Settings};
use mf_fixtures::{DamageSpec, damaged_sphere};
use mf_geometry::{ContentHash, Mesh};
use mf_io::{Format, ImportedModel, SourceInfo, SourcePrecision};
use mf_metrics::{GatedFrame, GpuSummary, Percentiles, frame_gate};

const BIG: usize = 5_000_000;
/// MF-RT-001: no UI frame over 50 ms while fill_holes runs on 5M triangles.
const FRAME_BUDGET: Duration = Duration::from_millis(50);
/// MF-RT-002: cancel returns within 250 ms.
const CANCEL_BUDGET: Duration = Duration::from_millis(250);

/// Swap in use and total (bytes), for an INCONCLUSIVE gate's message.
fn swap() -> Option<(u64, u64)> {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    (sys.total_swap() > 0).then(|| (sys.used_swap(), sys.total_swap()))
}

/// Page-ins so far; `None` where the OS doesn't count them (then a slow
/// frame can't be put down to swapping, and fails: see `frame_gate`).
fn page_ins() -> Option<u64> {
    // allow-swallow: unknown page-ins are `None`, which the gate treats as no excuse
    mf_metrics::page_ins().ok()
}

/// The page-ins between two readings.
fn page_ins_since(before: Option<u64>) -> Option<u64> {
    Some(page_ins()?.saturating_sub(before?))
}

/// MF-RT-001's gate with the swap policy (M4 Task 10 fix round 1, P1):
/// the budget never widens; a run whose over-budget frames all waited on
/// page-ins is INCONCLUSIVE and run exactly once more.
fn gate(what: &str, run: impl FnMut(u32) -> Vec<GatedFrame>) {
    match frame_gate(what, FRAME_BUDGET, swap, run) {
        Ok(log) => eprint!("{log}"),
        Err(log) => panic!("{log}"),
    }
}

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
        metadata: Default::default(),
        objects: vec![mf_io::threemf::ModelObject {
            metadata: Default::default(),
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
    opened_with(Settings::default())
}

fn opened_with(settings: Settings) -> (Harness<'static, MeshFixerApp>, egui_wgpu::RenderState) {
    let (mut harness, render_state) = empty_app_with(settings);
    harness.state_mut().import_model(big_model());
    wait_until(&mut harness, "5M analysis", Duration::from_secs(600), |a| {
        a.current_analysis().is_some() && a.viewport.frames_rendered > 0
    });
    (harness, render_state)
}

/// The app with nothing open (see [`opened`]).
fn empty_app() -> (Harness<'static, MeshFixerApp>, egui_wgpu::RenderState) {
    empty_app_with(Settings::default())
}

fn empty_app_with(settings: Settings) -> (Harness<'static, MeshFixerApp>, egui_wgpu::RenderState) {
    let mut setup = egui_kittest::wgpu::default_wgpu_setup();
    if let egui_wgpu::WgpuSetup::CreateNew(create) = &mut setup {
        create.device_descriptor = Arc::new(mf_desktop::gpu::device_descriptor);
    }
    let render_state =
        egui_kittest::wgpu::create_render_state(setup, egui_wgpu::RendererOptions::PREDICTABLE);
    let harness = Harness::builder()
        .with_size((1440.0, 900.0))
        .renderer(egui_kittest::wgpu::WgpuTestRenderer::from_render_state(
            render_state.clone(),
        ))
        .build_eframe(move |cc| MeshFixerApp::with_settings(cc, None, settings).unwrap());
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

    /// Returns how long encoding and submitting took, then waiting for the
    /// GPU (which also waits for anything workers submitted before).
    fn paint(&self, harness: &Harness<'_, MeshFixerApp>) -> (Duration, Duration) {
        let start = Instant::now();
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
        drop(renderer);
        let submitted = Instant::now();
        rs.device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(Duration::from_secs(10)),
            })
            .unwrap();
        (submitted - start, submitted.elapsed())
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
    // Repair holds the whole-object repairs (ui-plan.md §7).
    harness.get_by_label("Repair").click();
    harness.run_steps(2);
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
    if let Ok(dir) = std::env::var("MF_ARTIFACTS") {
        harness
            .render()
            .unwrap()
            .save(std::path::Path::new(&dir).join("progress-mid-run.png"))
            .unwrap();
    }
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

/// The GPU column of `FrameTimes`, in words.
fn gpu_summary(app: &MeshFixerApp) -> String {
    match app.frame_times.gpu() {
        GpuSummary::Unsupported => "timestamps unsupported".to_owned(),
        GpuSummary::Supported {
            measured,
            unusable,
            pending,
        } => format!(
            "{} of {} drawn frames measured ({unusable} unusable, {pending} pending): {}",
            measured.as_ref().map_or(0, |p| p.count()),
            measured.as_ref().map_or(0, |p| p.count()) + unusable + pending,
            summary("GPU", measured)
        ),
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Where one gated frame's time went: the app's update (`harness.step`,
/// which also encodes and submits the 3D view), painting egui (encode and
/// submit), and waiting for the GPU (including anything workers queued).
#[derive(Debug, Clone, Copy)]
struct FrameSplit {
    app: Duration,
    paint: Duration,
    gpu_wait: Duration,
    /// Page-ins while the frame ran (`None`: unknown).
    page_ins: Option<u64>,
}

impl FrameSplit {
    fn total(&self) -> Duration {
        self.app + self.paint + self.gpu_wait
    }
}

/// One whole frame with the camera moved, so the 3D view is redrawn.
fn orbit_frame(harness: &mut Harness<'_, MeshFixerApp>, painter: &Painter) -> FrameSplit {
    let paged = page_ins();
    let start = Instant::now();
    if let Some(cam) = harness.state_mut().camera.as_mut() {
        cam.orbit(0.01, 0.0);
    }
    harness.step();
    let app = start.elapsed();
    let (paint, gpu_wait) = painter.paint(harness);
    FrameSplit {
        app,
        paint,
        gpu_wait,
        page_ins: page_ins_since(paged),
    }
}

/// Times whole frames (see [`orbit_frame`]) until `done` holds after one,
/// that frame included. Each frame is labelled with what the app was doing
/// and where its time went (so a slow frame in CI says which phase it was).
fn gated_frames(
    harness: &mut Harness<'_, MeshFixerApp>,
    painter: &Painter,
    mut done: impl FnMut(&MeshFixerApp) -> bool,
) -> Vec<GatedFrame> {
    let mut frames = Vec::new();
    loop {
        let split = orbit_frame(harness, painter);
        let app = harness.state();
        let doing = match (app.job_progress(), app.shows_current_revision()) {
            (Some((stage, f)), _) => format!("{stage} {f:.3}"),
            (None, false) => "job done; new revision not drawn yet".to_owned(),
            (None, true) => "new revision drawn".to_owned(),
        };
        let at = format!(
            "{doing}; app update {:.1} ms, egui paint {:.1} ms, GPU wait {:.1} ms",
            ms(split.app),
            ms(split.paint),
            ms(split.gpu_wait)
        );
        frames.push(GatedFrame {
            time: split.total(),
            page_ins: split.page_ins,
            at,
        });
        if done(app) {
            return frames;
        }
        assert!(frames.len() < 100_000, "never finished");
    }
}

/// Reports `frames` (the gate itself is [`gate`]'s).
fn report(what: &str, frames: &[GatedFrame], app: &MeshFixerApp) {
    let times: Vec<Duration> = frames.iter().map(|f| f.time).collect();
    let worst = frames
        .iter()
        .max_by_key(|f| f.time)
        .expect("at least one frame");
    let last = frames.last().expect("at least one frame");
    let paged: u64 = frames.iter().filter_map(|f| f.page_ins).sum();
    eprintln!("{what}: {} frames, {paged} page-ins", frames.len());
    eprintln!("{}", summary("whole frame", Percentiles::of(times)));
    eprintln!(
        "worst frame {:.1} ms (at {}; {} page-ins)",
        ms(worst.time),
        worst.at,
        worst.page_ins.map_or("unknown".into(), |p| p.to_string())
    );
    eprintln!(
        "first frame drawing the new revision: {:.1} ms",
        ms(last.time)
    );
    let t = &app.frame_times;
    eprintln!("{}", summary("app CPU (update + encode/submit)", t.cpu()));
    eprintln!("{}", summary("app frame interval", t.interval()));
    eprintln!("GPU (3D view): {}", gpu_summary(app));
}

/// The surface bytes of `next`'s face chunks that a fill from `prev`
/// must rebuild: each chunk whose faces changed, and each holding a face
/// with a corner of a changed face (its smooth normal may differ).
fn touched_chunk_bytes(prev: &Mesh, next: &Mesh) -> u64 {
    use mf_geometry::{CHUNK, FaceId};
    assert!(
        next.moved_vertices(prev).is_empty(),
        "a fill moves no vertex"
    );
    let chunks = next.face_chunk_count();
    let slots = next.face_slots().max(prev.face_slots()) as usize;
    let mut corners = std::collections::HashSet::new();
    let mut changed = vec![false; chunks.max(prev.face_chunk_count())];
    for (i, c) in changed.iter_mut().enumerate() {
        if next.face_chunk_shared(prev, i) {
            continue;
        }
        *c = true;
        for f in i * CHUNK..((i + 1) * CHUNK).min(slots) {
            let (a, b) = (prev.face(FaceId(f as u32)), next.face(FaceId(f as u32)));
            if a != b {
                corners.extend(a.into_iter().chain(b).flatten());
            }
        }
    }
    let vertex = std::mem::size_of::<mf_render::SurfaceVertex>() as u64;
    (0..chunks)
        .filter(|&i| {
            changed[i]
                || next
                    .faces_in_chunk(i)
                    .any(|(_, t)| t.iter().any(|v| corners.contains(v)))
        })
        .map(|i| next.faces_in_chunk(i).count() as u64 * 3 * vertex)
        .sum()
}

/// MF-RT-001's acceptance test. A frame here is the whole frame: the app's
/// update (with the 3D view's encode and submit), egui's paint, and waiting
/// for the GPU to finish it all (see [`Painter`]). The camera orbits every
/// frame so the 5M mesh is redrawn every frame. Gated from the frame that
/// handles the Apply click through the first frame that draws the filled
/// revision: the commit, the re-analysis and the new revision's GPU
/// buffers all happen inside it.
#[test]
fn ui_frames_stay_under_50ms_while_fill_holes_runs_on_5m() {
    if !mf_render::offscreen::gpu_available() {
        mf_fixtures::skip::gpu_unavailable("ui_frames_stay_under_50ms_while_fill_holes_runs_on_5m");
        return;
    }
    let _heavy = heavy();
    gate("fill_holes on 5M", |_attempt| {
        let (mut harness, render_state) = opened();
        let painter = Painter::new(render_state, [1440, 900]);
        // Warm up: the mesh is on the GPU and egui's fonts are cached.
        for _ in 0..5 {
            orbit_frame(&mut harness, &painter);
        }
        let mut idle = Vec::new();
        for _ in 0..20 {
            idle.push(orbit_frame(&mut harness, &painter).total());
        }
        eprintln!(
            "{}",
            summary("idle whole frame (before the fill)", Percentiles::of(idle))
        );
        let labels = harness.state().session.history_labels().len();
        let rendered = harness.state().viewport.frames_rendered;
        let before = harness.state().active_mesh().unwrap().clone();

        // Repair holds the whole-object repairs (ui-plan.md §7).
        harness.get_by_label("Repair").click();
        harness.run_steps(2);
        harness.get_by_label("Fill all holes").click();
        harness.run_steps(2);
        harness.get_by_label("Apply").click();
        let applied = Instant::now();
        let mut fill_wall = None;
        let mut started = false;
        let mut committed_at = None;
        let mut rendered_while_running = 0;
        let mut n = 0;
        let frames = gated_frames(&mut harness, &painter, |app| {
            n += 1;
            started |= app.busy.is_some();
            assert!(started || n < 10, "the fill didn't start");
            if started && app.busy.is_none() && committed_at.is_none() {
                committed_at = Some(n);
                fill_wall = Some(applied.elapsed());
                rendered_while_running = app.viewport.frames_rendered - rendered;
            }
            started && app.busy.is_none() && app.shows_current_revision()
        });
        let app = harness.state();
        assert!(app.error.is_none(), "{:?}", app.error);
        assert_eq!(
            app.session.history_labels().len(),
            labels + 1,
            "the fill committed"
        );
        let committed_at = committed_at.expect("committed");
        assert!(
            rendered_while_running >= committed_at as u64 / 2,
            "the 3D view was redrawn as the camera moved"
        );
        eprintln!("commit frame: {:.1} ms", ms(frames[committed_at - 1].time));
        eprintln!(
            "fill wall time (Apply to commit): {:.2} s",
            fill_wall.expect("committed").as_secs_f64()
        );
        // MF-RENDER-003: the filled revision's GPU chunks, mostly shared.
        let view = app.object_views().into_iter().find(|v| v.active).unwrap();
        let (surface, _) = view.render.gpu.surface_vertex_count();
        let surface = (surface * std::mem::size_of::<mf_render::SurfaceVertex>()) as u64;
        let upload = view.render.gpu.last_upload();
        eprintln!(
            "filled revision uploaded {} of {surface} surface bytes, {} line bytes",
            upload.surface_bytes, upload.line_bytes
        );
        // The chunks the fill appended to, and (smooth shading, UI plan
        // step 3) the chunks holding a face with a corner on a changed
        // face (the filled holes' rims), whose normals now average in the
        // patches': exactly those, nothing else.
        let filled = app.active_mesh().unwrap();
        let expected = touched_chunk_bytes(&before, filled);
        eprintln!("expected re-upload (appended + rim chunks): {expected} bytes");
        assert!(
            upload.surface_bytes == expected && expected < surface,
            "a fill re-uploads only the chunks it appended to and the rims' chunks \
             ({expected} of {surface} bytes): {upload:?}"
        );
        assert!(
            committed_at >= 10,
            "too few frames while the fill ran to judge: {committed_at}"
        );
        report("fill_holes on 5M", &frames, app);
        frames
    });
}

/// MF-RT-001 for an import: gated from the frame that starts importing a
/// 5M-triangle STL through the first frame that draws it.
#[test]
fn ui_frames_stay_under_50ms_while_a_5m_file_imports() {
    if !mf_render::offscreen::gpu_available() {
        mf_fixtures::skip::gpu_unavailable("ui_frames_stay_under_50ms_while_a_5m_file_imports");
        return;
    }
    let _heavy = heavy();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.stl");
    mf_io::write_path(
        &big_mesh(),
        &path,
        Format::StlBinary,
        mf_io::threemf::Unit::Millimeter,
    )
    .unwrap();
    gate("5M STL import", |_attempt| {
        let (mut harness, render_state) = empty_app();
        let painter = Painter::new(render_state, [1440, 900]);
        for _ in 0..5 {
            orbit_frame(&mut harness, &painter);
        }
        harness.state_mut().open(path.clone());
        let mut started = false;
        let frames = gated_frames(&mut harness, &painter, |app| {
            started |= app.busy.is_some();
            started && app.busy.is_none() && app.shows_current_revision()
        });
        let app = harness.state();
        assert!(app.error.is_none(), "{:?}", app.error);
        assert_eq!(app.session.document().objects().len(), 1);
        report("5M STL import", &frames, app);
        frames
    });
}

/// MF-RT-001 for an STL export of a placed 5M object (M4 final review
/// I3): applying its transform is O(n), and runs in the export job. Gated
/// from the frame that starts the export (`export_to` included) through
/// the frame that sees it finish.
#[test]
fn ui_frames_stay_under_50ms_while_a_placed_5m_object_exports() {
    if !mf_render::offscreen::gpu_available() {
        mf_fixtures::skip::gpu_unavailable(
            "ui_frames_stay_under_50ms_while_a_placed_5m_object_exports",
        );
        return;
    }
    let _heavy = heavy();
    gate("placed 5M STL export", |_attempt| {
        let (mut harness, render_state) = empty_app();
        let mut model = big_model();
        model.objects[0].transform = mf_fixtures::rotation_translation(2, 30.0, [5.0, -3.0, 2.0]);
        harness.state_mut().import_model(model);
        wait_until(&mut harness, "5M analysis", Duration::from_secs(600), |a| {
            a.current_analysis().is_some() && a.viewport.frames_rendered > 0
        });
        let painter = Painter::new(render_state, [1440, 900]);
        for _ in 0..5 {
            orbit_frame(&mut harness, &painter);
        }
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("placed.stl");
        let paged = page_ins();
        let start = Instant::now();
        harness.state_mut().export_to(out.clone());
        let held = start.elapsed();
        let first = orbit_frame(&mut harness, &painter);
        let mut frames = vec![GatedFrame {
            time: held + first.total(),
            page_ins: page_ins_since(paged),
            at: format!(
                "export started; export_to {:.1} ms, app update {:.1} ms, egui paint {:.1} ms, \
                 GPU wait {:.1} ms",
                ms(held),
                ms(first.app),
                ms(first.paint),
                ms(first.gpu_wait)
            ),
        }];
        assert!(harness.state().busy.is_some(), "the export didn't start");
        frames.extend(gated_frames(&mut harness, &painter, |app| {
            app.busy.is_none()
        }));
        let app = harness.state();
        assert!(app.error.is_none(), "{:?}", app.error);
        assert!(out.is_file(), "the STL was written");
        assert!(
            frames.len() >= 10,
            "too few frames while the export ran to judge: {}",
            frames.len()
        );
        report("placed 5M STL export", &frames, app);
        frames
    });
}

/// Clicks Cancel and returns how long until the job has stopped and the
/// app is idle (MF-RT-002: within 250 ms).
fn cancel_latency(harness: &mut Harness<'_, MeshFixerApp>) -> Duration {
    harness.get_by_label("Cancel").click();
    let start = Instant::now();
    wait_until(harness, "cancel", Duration::from_secs(60), |a| {
        a.busy.is_none()
    });
    let took = start.elapsed();
    assert_eq!(
        harness.state().status.as_deref(),
        Some("Cancelled: document unchanged")
    );
    took
}

/// A fill applied right after a 5M import, while the import's analysis
/// (and its render vertices) are still being built, still stops within
/// 250 ms of Cancel.
#[test]
fn cancel_is_prompt_when_a_fill_is_applied_right_after_import() {
    if !mf_render::offscreen::gpu_available() {
        mf_fixtures::skip::gpu_unavailable(
            "cancel_is_prompt_when_a_fill_is_applied_right_after_import",
        );
        return;
    }
    let _heavy = heavy();
    let (mut harness, _) = empty_app_with(Settings {
        threads: Some(4),
        ..Settings::default()
    });
    harness.state_mut().import_model(big_model());
    harness.step();
    assert!(harness.state().is_analyzing());
    start_fill(&mut harness);
    wait_until(
        &mut harness,
        "triangulating",
        Duration::from_secs(60),
        |a| {
            assert!(
                a.busy.is_some(),
                "the fill finished before it could be cancelled"
            );
            a.job_progress()
                .is_some_and(|(stage, _)| stage == "triangulating" || stage == "refining")
        },
    );
    let analysing = harness.state().is_analyzing();
    let took = cancel_latency(&mut harness);
    eprintln!(
        "cancel after apply-right-after-import: {took:?} (analysis still running: {analysing})"
    );
    assert!(took <= CANCEL_BUDGET, "took {took:?}");
}

/// Switching objects during a fill, as clicks on tree rows do, queues
/// work on the background pool: the other object's analysis, the filled
/// object's render vertices, and (selecting the 5M object's main body) a
/// 5M-face selection overlay each time. Cancel still stops the fill within
/// 250 ms: none of that work may end up inside the fill (rayon lets a
/// thread waiting in a join run any queued job of its pool). The pool is
/// small (4 threads) so the fill's threads wait in joins often.
#[test]
fn cancel_is_prompt_after_switching_objects_during_a_fill() {
    if !mf_render::offscreen::gpu_available() {
        mf_fixtures::skip::gpu_unavailable(
            "cancel_is_prompt_after_switching_objects_during_a_fill",
        );
        return;
    }
    let _heavy = heavy();
    let (mut harness, _) = opened_with(Settings {
        threads: Some(4),
        ..Settings::default()
    });
    let big = harness.state().active.unwrap();
    // A second, small object, added without making it active.
    let (ids, _) = harness.state_mut().session.import_model(ImportedModel {
        metadata: Default::default(),
        objects: vec![mf_io::threemf::ModelObject {
            metadata: Default::default(),
            name: "cube".into(),
            mesh: mf_fixtures::cube(1.0),
            transform: mf_io::threemf::IDENTITY,
        }],
        ..big_model()
    });
    let cube = ids[0];
    harness.run_steps(5);
    assert_eq!(harness.state().active, Some(big));
    start_fill(&mut harness);
    wait_until(
        &mut harness,
        "triangulating",
        Duration::from_secs(60),
        |a| {
            assert!(
                a.busy.is_some(),
                "the fill finished before it could be cancelled"
            );
            a.job_progress()
                .is_some_and(|(stage, _)| stage == "triangulating" || stage == "refining")
        },
    );
    for _ in 0..4 {
        harness.state_mut().set_active(cube);
        harness.run_steps(2);
        let app = harness.state_mut();
        app.set_active(big);
        app.selection = Some(mf_selection::Selection {
            object: big,
            revision: app.session.current(),
            kind: mf_selection::SelectionKind::Component,
            ids: vec![0],
        });
        harness.run_steps(2);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        harness.state().busy.is_some(),
        "the fill finished before it could be cancelled"
    );
    let took = cancel_latency(&mut harness);
    eprintln!("cancel after switching objects during a fill: {took:?}");
    assert!(took <= CANCEL_BUDGET, "took {took:?}");
}

/// Fix round 2, N1 (MF-RT-001): hovering the view while another thread
/// computes a 5M mesh's topology never stalls a UI frame. Picking
/// readiness is checked as the pointer moves, and must not wait for the
/// topology lock the computation holds.
#[test]
fn ui_frames_stay_under_50ms_while_hovering_during_a_5m_topology_build() {
    if !mf_render::offscreen::gpu_available() {
        mf_fixtures::skip::gpu_unavailable(
            "ui_frames_stay_under_50ms_while_hovering_during_a_5m_topology_build",
        );
        return;
    }
    let _heavy = heavy();
    gate("hover during a 5M topology build", |_attempt| {
        let (mut harness, _render_state) = empty_app();
        // A small object first: the view has a camera and draws, so the
        // pointer over it asks for hover picks.
        let mut small = big_model();
        small.objects[0].name = "small".into();
        small.objects[0].mesh = mf_fixtures::cube(1.0);
        harness.state_mut().import_model(small);
        wait_until(
            &mut harness,
            "small object ready",
            Duration::from_secs(60),
            |a| a.current_analysis().is_some() && a.camera.is_some() && a.picking_ready(),
        );
        let view = harness.state().viewport.last_rect.unwrap();

        // The 5M object, its topology not yet computed (earlier tests may
        // have cached it on the shared mesh).
        let big = big_mesh();
        big.release_topology();
        harness.state_mut().import_model(big_model());
        let computing = std::thread::spawn(move || {
            big.topology();
        });
        let start = Instant::now();
        let mut frames = Vec::new();
        let mut i = 0;
        while !(computing.is_finished() && harness.state().picking_ready()) {
            assert!(start.elapsed() < Duration::from_secs(600), "timed out");
            let p = view.center() + egui::vec2((i % 40) as f32 * 4.0 - 80.0, (i % 7) as f32 * 3.0);
            harness.event(egui::Event::PointerMoved(p));
            let paged = page_ins();
            let t = Instant::now();
            harness.step();
            frames.push(GatedFrame {
                time: t.elapsed(),
                page_ins: page_ins_since(paged),
                at: format!("frame {i} of the hover"),
            });
            i += 1;
            std::thread::sleep(Duration::from_millis(2));
        }
        computing.join().unwrap();
        frames
    });
}
