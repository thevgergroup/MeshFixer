//! Idle soak: the real app (eframe + winit + wgpu, a real window) with a
//! model open in metres and one tool open, left alone in a chosen window
//! state, printing its heap and frame counts every few seconds.
//!
//! `MF_SOAK_MODEL=path MF_SOAK_TOOL=hollow MF_SOAK_STATE=visible|minimized|hidden
//!  cargo run --release --example idle_soak`
//!
//! Sample the process from outside with `footprint <pid>` too.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use eframe::egui;
use mf_desktop::{MeshFixerApp, Settings};
use mf_metrics::CountingAlloc;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

#[derive(Default)]
struct Counters {
    ui: AtomicU64,
    logic: AtomicU64,
}

struct Soak {
    app: MeshFixerApp,
    tool: String,
    state: String,
    stage: u8,
    since: Instant,
    counters: Arc<Counters>,
}

impl eframe::App for Soak {
    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.counters.logic.fetch_add(1, Ordering::Relaxed);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.counters.ui.fetch_add(1, Ordering::Relaxed);
        self.app.ui(ui, frame);
        let ctx = ui.ctx().clone();
        match self.stage {
            0 if self.app.current_analysis().is_some() && self.app.picking_ready() => {
                self.app.open_rail_tool(&self.tool);
                eprintln!(
                    "soak: tool {} open (active {}, status {:?})",
                    self.tool, self.app.active_tool, self.app.status
                );
                self.stage = 1;
                self.since = Instant::now();
                ctx.request_repaint_after(Duration::from_secs(6));
            }
            1 if self.since.elapsed() > Duration::from_secs(5) => {
                self.stage = 2;
                match self.state.as_str() {
                    "minimized" => ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
                    "hidden" => ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false)),
                    _ => {}
                }
                eprintln!(
                    "soak: now {} (active {:?}, hollow for {:?} wall {:?}, busy {:?})",
                    self.state,
                    self.app.active,
                    self.app.hollow.for_object,
                    self.app.hollow.wall,
                    self.app.busy
                );
            }
            0 => ctx.request_repaint_after(Duration::from_millis(50)),
            1 => ctx.request_repaint_after(Duration::from_secs(1)),
            _ => {}
        }
    }
}

fn main() -> eframe::Result {
    let model = std::env::var("MF_SOAK_MODEL").expect("MF_SOAK_MODEL");
    let tool = std::env::var("MF_SOAK_TOOL").unwrap_or_else(|_| "hollow".into());
    let state = std::env::var("MF_SOAK_STATE").unwrap_or_else(|_| "visible".into());
    let counters = Arc::new(Counters::default());
    {
        let counters = counters.clone();
        std::thread::spawn(move || {
            let start = Instant::now();
            loop {
                std::thread::sleep(Duration::from_secs(5));
                eprintln!(
                    "sample t={:>5}s heap={} MB ui={} logic={} rss={:?}",
                    start.elapsed().as_secs(),
                    mf_metrics::live_bytes() >> 20,
                    counters.ui.load(Ordering::Relaxed),
                    counters.logic.load(Ordering::Relaxed),
                    mf_metrics::process_rss_bytes().map(|b| b >> 20),
                );
            }
        });
    }
    let mut imported = mf_io::read_model(std::path::Path::new(&model)).expect("model");
    imported.unit = Some(mf_units::Unit::Meter);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("MeshFixer soak")
            .with_inner_size([1440.0, 900.0]),
        wgpu_options: mf_desktop::gpu::wgpu_configuration(),
        ..Default::default()
    };
    eframe::run_native(
        "MeshFixer soak",
        options,
        Box::new(move |cc| {
            let dir = std::env::temp_dir().join(format!("mf-soak-{}", std::process::id()));
            let settings = Settings {
                config_dir: Some(dir),
                ..Settings::default()
            };
            let mut app = MeshFixerApp::with_settings(cc, None, settings)?;
            app.import_model(imported);
            Ok(Box::new(Soak {
                app,
                tool,
                state,
                stage: 0,
                since: Instant::now(),
                counters,
            }))
        }),
    )
}
